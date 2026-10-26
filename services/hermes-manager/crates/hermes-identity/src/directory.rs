//! Read-only Keycloak Admin REST lookup with a private service credential.
//! Responses are bounded; provider failures are `Unavailable`, never empty.
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hermes_core::app::directory::{DirectoryResults, DirectoryUser, IdentityDirectory};
use hermes_core::app::error::AppError;
use hermes_core::domain::identity::{Identity, validate_issuer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use url::Url;
use zeroize::Zeroizing;

pub struct KeycloakDirectory {
    issuer: String,
    token_url: Url,
    users_url: Url,
    client_id: String,
    secret: Zeroizing<String>,
    client: reqwest::Client,
    token: Mutex<Option<(Zeroizing<String>, Instant)>>,
}
#[derive(Deserialize)]
struct Token {
    access_token: String,
    expires_in: u64,
    token_type: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    id: String,
    username: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    email: Option<String>,
    #[serde(default)]
    enabled: bool,
}
async fn bounded(mut response: reqwest::Response, maximum: usize) -> Result<Vec<u8>, AppError> {
    if !response.status().is_success() {
        return Err(AppError::unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::unavailable())?
    {
        if bytes.len() + chunk.len() > maximum {
            return Err(AppError::unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
impl KeycloakDirectory {
    pub fn new(
        issuer: &str,
        client_id: String,
        secret: Zeroizing<String>,
        client: reqwest::Client,
    ) -> Result<Arc<Self>, AppError> {
        validate_issuer(issuer).map_err(|_| AppError::invalid())?;
        let (base, realm) = issuer
            .rsplit_once("/realms/")
            .ok_or_else(AppError::invalid)?;
        if matches!(realm, "." | "..")
            || !realm
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
            || realm.is_empty()
            || realm.contains('/')
            || client_id.is_empty()
            || client_id.len() > 255
            || secret.trim().is_empty()
        {
            return Err(AppError::invalid());
        }
        let token_url = Url::parse(&format!("{issuer}/protocol/openid-connect/token"))
            .map_err(|_| AppError::invalid())?;
        let users_url = Url::parse(&format!("{base}/admin/realms/{realm}/users"))
            .map_err(|_| AppError::invalid())?;
        Ok(Arc::new(Self {
            issuer: issuer.into(),
            token_url,
            users_url,
            client_id,
            secret,
            client,
            token: Mutex::new(None),
        }))
    }
    async fn token(&self) -> Result<Zeroizing<String>, AppError> {
        let mut cached = self.token.lock().await;
        if let Some((value, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(value.clone());
        }
        let response = self
            .client
            .post(self.token_url.clone())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.secret.trim()),
            ])
            .send()
            .await
            .map_err(|_| AppError::unavailable())?;
        let bytes = Zeroizing::new(bounded(response, 32768).await?);
        let token: Token = serde_json::from_slice(&bytes).map_err(|_| AppError::unavailable())?;
        if !token.token_type.eq_ignore_ascii_case("bearer")
            || token.access_token.is_empty()
            || token.access_token.len() > 16384
            || token.expires_in < 1
        {
            return Err(AppError::unavailable());
        }
        let value = Zeroizing::new(token.access_token);
        *cached = Some((
            value.clone(),
            Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(10).min(60)),
        ));
        Ok(value)
    }
    async fn page(
        &self,
        q: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<DirectoryResults, AppError> {
        if q.len() > 128 || !(1..=100).contains(&limit) {
            return Err(AppError::invalid());
        }
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(
            serde_json::to_vec(&(&self.issuer, q, limit)).map_err(|_| AppError::invalid())?,
        ));
        let offset = if let Some(cursor) = after {
            if cursor.len() > 256 {
                return Err(AppError::invalid());
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(cursor)
                .map_err(|_| AppError::invalid())?;
            let (bound, offset): (String, u32) =
                serde_json::from_slice(&decoded).map_err(|_| AppError::invalid())?;
            if bound != digest {
                return Err(AppError::invalid());
            }
            offset
        } else {
            0
        };
        let token = self.token().await?;
        let response = self
            .client
            .get(self.users_url.clone())
            .bearer_auth(token.as_str())
            .query(&[
                ("search", q),
                ("first", &offset.to_string()),
                ("max", &(u32::from(limit) + 1).to_string()),
                ("briefRepresentation", "true"),
            ])
            .send()
            .await
            .map_err(|_| AppError::unavailable())?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            *self.token.lock().await = None;
            return Err(AppError::unavailable());
        }
        let users: Vec<User> = serde_json::from_slice(&bounded(response, 256 * 1024).await?)
            .map_err(|_| AppError::unavailable())?;
        if users.len() > usize::from(limit) + 1 {
            return Err(AppError::unavailable());
        }
        let next_cursor = if users.len() > usize::from(limit) {
            let next = offset
                .checked_add(u32::from(limit))
                .ok_or_else(AppError::invalid)?;
            Some(
                URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(&(digest, next)).map_err(|_| AppError::invalid())?),
            )
        } else {
            None
        };
        let mut items = Vec::new();
        for user in users.into_iter().take(usize::from(limit)) {
            let identity = Identity {
                issuer: self.issuer.clone(),
                subject: user.id,
            };
            identity
                .validate(&self.issuer)
                .map_err(|_| AppError::unavailable())?;
            for label in [
                &user.username,
                &user.first_name,
                &user.last_name,
                &user.email,
            ]
            .into_iter()
            .flatten()
            {
                if label.len() > 320 || label.chars().any(char::is_control) {
                    return Err(AppError::unavailable());
                }
            }
            let name = [user.first_name, user.last_name]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            items.push(DirectoryUser {
                identity,
                username: user.username,
                display_name: (!name.is_empty()).then_some(name),
                email: user.email,
                enabled: user.enabled,
            });
        }
        Ok(DirectoryResults { items, next_cursor })
    }
}
#[async_trait]
impl IdentityDirectory for KeycloakDirectory {
    async fn search(
        &self,
        query: &str,
        after: Option<&str>,
        limit: u16,
    ) -> Result<DirectoryResults, AppError> {
        tokio::time::timeout(Duration::from_secs(5), self.page(query, after, limit))
            .await
            .map_err(|_| AppError::unavailable())?
    }
}
