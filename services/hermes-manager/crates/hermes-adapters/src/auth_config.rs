//! Reference-only authentication process configuration, shared by both services.
use crate::{
    auth::AccessTokenVerifier,
    browser_auth::{BrowserAuth, BrowserConfig},
    oidc::{OidcConfig, OidcProvider},
    session_crypto::{Purpose, Sealer},
    sessions::PgSessions,
};
use hermes_application::{AdminPolicy, Error, InstallationScope};
use serde::Deserialize;
use sqlx::postgres::PgConnectOptions;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};
use zeroize::Zeroizing;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionConfig {
    pub database_url_file: PathBuf,
    pub public_origin: String,
    pub other_origin: String,
    pub client_id: String,
    pub client_secret_file: Option<PathBuf>,
    pub scopes: Vec<String>,
    pub revocation_url: Option<String>,
    pub encryption_keyring_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Keyring {
    active_key_id: String,
    key_files: BTreeMap<String, PathBuf>,
}

pub async fn read(path: &Path, maximum: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::Unavailable)?;
    // Take bounds actual reads, including pseudo files and changes after metadata.
    use tokio::io::AsyncReadExt;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Error::Unavailable)?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}

impl SessionConfig {
    pub async fn build(
        self,
        scope: InstallationScope,
        purpose: Purpose,
        tokens: AccessTokenVerifier,
        http: reqwest::Client,
        admin: Option<AdminPolicy>,
    ) -> Result<BrowserAuth, Error> {
        let url = read(&self.database_url_file, 16 * 1024).await?;
        let options = PgConnectOptions::from_str(
            std::str::from_utf8(&url)
                .map_err(|_| Error::Invalid)?
                .trim(),
        )
        .map_err(|_| Error::Invalid)?;
        let sessions = PgSessions::connect(options, scope.clone(), purpose).await?;
        let keyring: Keyring =
            serde_json::from_slice(&read(&self.encryption_keyring_file, 16 * 1024).await?)
                .map_err(|_| Error::Invalid)?;
        if keyring.key_files.len() > 16 {
            return Err(Error::Invalid);
        }
        let mut keys = BTreeMap::new();
        for (id, path) in keyring.key_files {
            keys.insert(id, read(&path, 32).await?.to_vec());
        }
        let secret = match self.client_secret_file {
            Some(path) => Some(
                String::from_utf8(read(&path, 4096).await?.to_vec())
                    .map_err(|_| Error::Invalid)?
                    .trim()
                    .into(),
            ),
            None => None,
        };
        let path = match purpose {
            Purpose::Administration => "/auth/oidc/callback",
            Purpose::Router => "/_platform/oidc/callback",
        };
        let callback = url::Url::parse(&self.public_origin)
            .map_err(|_| Error::Invalid)?
            .join(path)
            .map_err(|_| Error::Invalid)?
            .to_string();
        let provider = OidcProvider::new(
            OidcConfig {
                issuer: scope.issuer,
                client_id: self.client_id,
                callback_url: callback,
                scopes: self.scopes,
                client_secret: secret,
                revocation_url: self.revocation_url,
            },
            http,
        )?;
        BrowserAuth::new(
            sessions,
            provider,
            tokens,
            Sealer::new(keyring.active_key_id, keys)?,
            BrowserConfig {
                public_origin: self.public_origin,
                other_origin: self.other_origin,
            },
            admin,
        )
    }
}
