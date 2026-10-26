//! Directory lookup against a fake Keycloak over real TLS. Results are
//! candidates only; provider failures are `Unavailable`, never empty.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Form, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hermes_core::app::directory::IdentityDirectory;
use hermes_core::domain::ErrorKind;
use hermes_identity::directory::KeycloakDirectory;
use hermes_identity::http_client;
use hermes_testkit::{Secrets, serve_tls};
use serde_json::json;
use zeroize::Zeroizing;

const TOKEN_PATH: &str = "/realms/agents/protocol/openid-connect/token";
const USERS_PATH: &str = "/admin/realms/agents/users";

fn directory(secrets: &Secrets, url: &str, secret: &str) -> Arc<KeycloakDirectory> {
    KeycloakDirectory::new(
        &format!("{url}/realms/agents"),
        "directory-reader".into(),
        Zeroizing::new(secret.into()),
        http_client(Some(&secrets.read("ca.crt"))).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn directory_uses_a_cached_service_token_bound_cursors_and_allowlisted_labels() {
    let secrets = Secrets::new().await;
    let tokens = Arc::new(AtomicUsize::new(0));
    let users = Arc::new(AtomicUsize::new(0));
    let (token_count, user_count) = (tokens.clone(), users.clone());
    let router = Router::new()
        .route(
            TOKEN_PATH,
            post(move |Form(form): Form<HashMap<String, String>>| {
                let count = token_count.clone();
                async move {
                    assert_eq!(form["grant_type"], "client_credentials");
                    assert_eq!(form["client_id"], "directory-reader");
                    assert_eq!(form["client_secret"], "synthetic-directory-secret");
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"access_token": "synthetic-service-token", "expires_in": 300, "token_type": "Bearer"}))
                }
            }),
        )
        .route(
            USERS_PATH,
            get(move |headers: HeaderMap, Query(query): Query<HashMap<String, String>>| {
                let count = user_count.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer synthetic-service-token");
                    assert_eq!(query["search"], "fixture & owner");
                    assert_eq!(query["max"], "2");
                    assert_eq!(query["briefRepresentation"], "true");
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(if query["first"] == "0" {
                        json!([
                            {"id": "owner-a", "username": "fixture-owner", "firstName": "Fixture",
                             "lastName": "Owner", "email": "fixture@example.org", "enabled": true,
                             "attributes": {"private": "never-return-this"}},
                            {"id": "owner-b", "enabled": true}
                        ])
                    } else {
                        json!([{"id": "owner-b", "enabled": false}])
                    })
                }
            }),
        );
    let provider = serve_tls(&secrets, router).await;
    let issuer = format!("{}/realms/agents", provider.url);
    let directory = directory(&secrets, &provider.url, "synthetic-directory-secret");

    let first = directory.search("fixture & owner", None, 1).await.unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].identity.subject, "owner-a");
    assert_eq!(first.items[0].identity.issuer, issuer);
    assert_eq!(
        first.items[0].display_name.as_deref(),
        Some("Fixture Owner")
    );
    assert!(first.items[0].enabled);
    assert!(
        !serde_json::to_string(&first.items)
            .unwrap()
            .contains("never-return-this")
    );

    let cursor = first.next_cursor.as_deref().unwrap();
    let changed = directory
        .search("changed", Some(cursor), 1)
        .await
        .unwrap_err();
    assert_eq!(changed.kind, ErrorKind::Invalid);

    let next = directory
        .search("fixture & owner", Some(cursor), 1)
        .await
        .unwrap();
    assert_eq!(next.items[0].identity.subject, "owner-b");
    assert!(!next.items[0].enabled);
    assert!(next.next_cursor.is_none());
    assert_eq!(tokens.load(Ordering::SeqCst), 1);
    assert_eq!(users.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn directory_provider_denial_is_unavailable_not_an_empty_directory() {
    let secrets = Secrets::new().await;
    let router = Router::new().route(TOKEN_PATH, post(|| async { StatusCode::FORBIDDEN }));
    let provider = serve_tls(&secrets, router).await;
    let directory = directory(&secrets, &provider.url, "synthetic-secret");
    let error = directory.search("", None, 50).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
}

#[tokio::test]
async fn directory_refuses_oversized_or_control_labelled_users() {
    let secrets = Secrets::new().await;
    let router = Router::new()
        .route(
            TOKEN_PATH,
            post(|| async {
                Json(json!({"access_token": "t", "expires_in": 60, "token_type": "bearer"}))
            }),
        )
        .route(
            USERS_PATH,
            get(|| async {
                Json(json!([{"id": "owner-a", "username": "line\nbreak", "enabled": true}]))
            }),
        );
    let provider = serve_tls(&secrets, router).await;
    let directory = directory(&secrets, &provider.url, "synthetic-secret");
    let error = directory.search("", None, 10).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
}

#[test]
fn directory_configuration_is_validated() {
    let client = reqwest::Client::new();
    for issuer in [
        "https://idp.example.org/no-realm",
        "https://idp.example.org/realms/..",
        "http://idp.example.org/realms/a",
    ] {
        assert!(
            KeycloakDirectory::new(
                issuer,
                "reader".into(),
                Zeroizing::new("s".into()),
                client.clone()
            )
            .is_err(),
            "{issuer}"
        );
    }
    assert!(
        KeycloakDirectory::new(
            "https://idp.example.org/realms/a",
            String::new(),
            Zeroizing::new("s".into()),
            client.clone()
        )
        .is_err()
    );
    assert!(
        KeycloakDirectory::new(
            "https://idp.example.org/realms/a",
            "reader".into(),
            Zeroizing::new("  ".into()),
            client
        )
        .is_err()
    );
}
