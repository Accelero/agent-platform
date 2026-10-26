//! A fake Keycloak realm: discovery, authorization, token and JWKS endpoints,
//! with switchable faults. Tokens are signed with the fixture key.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Form, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{Running, Secrets, serve_tls};

/// What the provider gets wrong on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    #[default]
    None,
    /// Discovery advertises only pairwise subjects.
    Pairwise,
    /// Refresh tokens are rejected.
    Revoked,
    /// A refresh returns tokens for another subject.
    RefreshSubject,
    /// The access token names another subject than the ID token.
    AccessSubject,
    Audience,
    Client,
    /// The access token was issued 121 seconds ago.
    Stale,
    /// Tokens are signed with a key outside the JWKS.
    Signature,
    Nonce,
    /// The ID token's `at_hash` does not match the access token.
    Hash,
}

struct Grant {
    nonce: String,
    challenge: String,
    subject: String,
}

struct ProviderState {
    signer: EncodingKey,
    foreign_signer: EncodingKey,
    jwk: Value,
    client_id: String,
    audience: String,
    issuer: Mutex<String>,
    subject: Mutex<String>,
    fault: Mutex<Fault>,
    codes: Mutex<HashMap<String, Grant>>,
    refresh: Mutex<HashMap<String, String>>,
    exchanges: AtomicUsize,
    refreshes: AtomicUsize,
}

pub struct FakeProvider {
    state: Arc<ProviderState>,
    pub issuer: String,
    _server: Running,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// 64 random hex characters, like the manager's opaque handles.
fn handle() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl FakeProvider {
    /// Starts a realm at `https://127.0.0.1:<port>/realms/agents` issuing
    /// tokens for `client_id` with audience `audience`.
    pub async fn start(secrets: &Secrets, client_id: &str, audience: &str) -> Self {
        let state = Arc::new(ProviderState {
            signer: secrets.signer.clone(),
            foreign_signer: secrets.foreign_signer.clone(),
            jwk: secrets.jwk.clone(),
            client_id: client_id.into(),
            audience: audience.into(),
            issuer: Mutex::new(String::new()),
            subject: Mutex::new("owner-a".into()),
            fault: Mutex::new(Fault::None),
            codes: Mutex::new(HashMap::new()),
            refresh: Mutex::new(HashMap::new()),
            exchanges: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route(
                "/realms/agents/.well-known/openid-configuration",
                get(metadata),
            )
            .route("/realms/agents/jwks", get(jwks))
            .route("/realms/agents/authorize", get(authorize))
            .route("/realms/agents/token", post(token))
            .with_state(state.clone());
        let server = serve_tls(secrets, router).await;
        let issuer = format!("{}/realms/agents", server.url);
        *state.issuer.lock().unwrap() = issuer.clone();
        Self {
            state,
            issuer,
            _server: server,
        }
    }

    pub fn jwks_url(&self) -> String {
        format!("{}/jwks", self.issuer)
    }

    pub fn set_subject(&self, subject: &str) {
        *self.state.subject.lock().unwrap() = subject.into();
    }

    pub fn set_fault(&self, fault: Fault) {
        *self.state.fault.lock().unwrap() = fault;
    }

    pub fn exchanges(&self) -> usize {
        self.state.exchanges.load(Ordering::SeqCst)
    }

    pub fn refreshes(&self) -> usize {
        self.state.refreshes.load(Ordering::SeqCst)
    }

    /// The fixture signing key, for tokens with crafted headers.
    pub fn signer(&self) -> &EncodingKey {
        &self.state.signer
    }

    /// Signs arbitrary access-token claims with the fixture key.
    pub fn sign(&self, claims: &Value) -> String {
        sign(&self.state.signer, claims)
    }

    /// Claims of a valid access token for `subject`, ready to be altered.
    pub fn access_claims(&self, subject: &str) -> Value {
        let now = now();
        json!({"iss": self.issuer, "sub": subject, "aud": self.state.audience, "azp": self.state.client_id,
               "iat": now, "exp": now + 300, "nbf": now - 1, "typ": "Bearer", "sid": "provider-session-one"})
    }

    /// A valid signed access token for `subject`.
    pub fn access_token(&self, subject: &str) -> String {
        self.sign(&self.access_claims(subject))
    }
}

fn sign(key: &EncodingKey, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".into());
    encode(&header, claims, key).expect("sign")
}

async fn metadata(State(p): State<Arc<ProviderState>>) -> Json<Value> {
    let issuer = p.issuer.lock().unwrap().clone();
    let subjects = if *p.fault.lock().unwrap() == Fault::Pairwise {
        "pairwise"
    } else {
        "public"
    };
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": [subjects],
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks(State(p): State<Arc<ProviderState>>) -> Json<Value> {
    Json(json!({"keys": [p.jwk.clone()]}))
}

async fn authorize(
    State(p): State<Arc<ProviderState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(q.get("client_id"), Some(&p.client_id));
    assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(
        q.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    let code = handle();
    let grant = Grant {
        nonce: q["nonce"].clone(),
        challenge: q["code_challenge"].clone(),
        subject: p.subject.lock().unwrap().clone(),
    };
    p.codes.lock().unwrap().insert(code.clone(), grant);
    let mut callback = Url::parse(&q["redirect_uri"]).expect("redirect uri");
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &q["state"]);
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, callback.to_string())],
    )
        .into_response()
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant"})),
    )
        .into_response()
}

async fn token(
    State(p): State<Arc<ProviderState>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let fault = *p.fault.lock().unwrap();
    let refreshing = form["grant_type"] == "refresh_token";
    let (subject, nonce) = if refreshing {
        p.refreshes.fetch_add(1, Ordering::SeqCst);
        let subject = p.refresh.lock().unwrap().remove(&form["refresh_token"]);
        match subject {
            Some(subject) if fault != Fault::Revoked => (subject, None),
            _ => return invalid_grant(),
        }
    } else {
        assert_eq!(form["grant_type"], "authorization_code");
        p.exchanges.fetch_add(1, Ordering::SeqCst);
        let Some(grant) = p.codes.lock().unwrap().remove(&form["code"]) else {
            return invalid_grant();
        };
        if URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()))
            != grant.challenge
        {
            return invalid_grant();
        }
        (grant.subject, Some(grant.nonce))
    };
    let subject = if fault == Fault::RefreshSubject && refreshing {
        "owner-b".to_owned()
    } else {
        subject
    };
    let issuer = p.issuer.lock().unwrap().clone();
    let now = now();
    let mut access = json!({"iss": issuer, "sub": subject, "aud": p.audience, "azp": p.client_id,
        "iat": now, "exp": now + 300, "nbf": now - 1, "typ": "Bearer", "sid": "provider-session-one"});
    match fault {
        Fault::AccessSubject => access["sub"] = json!("foreign"),
        Fault::Audience => access["aud"] = json!("wrong-api"),
        Fault::Client => access["azp"] = json!("external-panel"),
        Fault::Stale => access["iat"] = json!(now - 121),
        _ => {}
    }
    let signer = if fault == Fault::Signature {
        &p.foreign_signer
    } else {
        &p.signer
    };
    let access = sign(signer, &access);
    let digest = Sha256::digest(access.as_bytes());
    let mut id = json!({"iss": issuer, "sub": subject, "aud": p.client_id, "iat": now, "exp": now + 300,
        "at_hash": URL_SAFE_NO_PAD.encode(&digest[..16])});
    if let Some(nonce) = nonce {
        id["nonce"] = json!(if fault == Fault::Nonce {
            "incorrect".to_owned()
        } else {
            nonce
        });
    }
    if fault == Fault::Hash {
        id["at_hash"] = json!(URL_SAFE_NO_PAD.encode([0u8; 16]));
    }
    let id = sign(signer, &id);
    let refresh = handle();
    p.refresh.lock().unwrap().insert(refresh.clone(), subject);
    Json(
        json!({"access_token": access, "id_token": id, "token_type": "Bearer",
                "expires_in": 300, "refresh_token": refresh}),
    )
    .into_response()
}
