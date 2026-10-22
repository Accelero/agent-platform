//! Real TLS provider boundary; no directory credential or label becomes an admin grant.
#[allow(dead_code)]
mod support;
use axum::{
    Json, Router,
    extract::{Form, Query},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use hermes_adapters::{auth::http_client, directory::KeycloakDirectory};
use hermes_application::{Error, directory::IdentityDirectory};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::{Secrets, start};

#[tokio::test]
async fn directory_uses_private_service_tokens_bound_pages_and_allowlisted_labels() {
    let secrets = Secrets::new().await;
    let tokens = Arc::new(AtomicUsize::new(0));
    let users = Arc::new(AtomicUsize::new(0));
    let token_count = tokens.clone();
    let user_count = users.clone();
    let provider=start(&secrets,Router::new()
        .route("/realms/agents/protocol/openid-connect/token",post(move |Form(form):Form<HashMap<String,String>>| {let count=token_count.clone();async move {
            assert_eq!(form["grant_type"],"client_credentials");assert_eq!(form["client_id"],"directory-reader");assert_eq!(form["client_secret"],"synthetic-directory-secret"); count.fetch_add(1,Ordering::SeqCst);
            Json(json!({"access_token":"synthetic-service-token","expires_in":300,"token_type":"Bearer"}))
        }}))
        .route("/admin/realms/agents/users",get(move |headers:HeaderMap,Query(query):Query<HashMap<String,String>>| {let count=user_count.clone();async move {
            assert_eq!(headers["authorization"],"Bearer synthetic-service-token");assert_eq!(query["search"],"fixture & owner");assert_eq!(query["max"],"2");assert_eq!(query["briefRepresentation"],"true");count.fetch_add(1,Ordering::SeqCst);
            Json(if query["first"]=="0" {json!([{"id":"owner-a","username":"fixture-owner","firstName":"Fixture","lastName":"Owner","email":"fixture@example.org","enabled":true,"attributes":{"private":"never-return-this"}},{"id":"owner-b","enabled":true}])}else {json!([{"id":"owner-b","enabled":false}])})
        }})),false).await;
    let issuer = format!("{}/realms/agents", provider.url);
    let directory = KeycloakDirectory::new(
        &issuer,
        "directory-reader".into(),
        zeroize::Zeroizing::new("synthetic-directory-secret".into()),
        http_client(Some(&secrets.read("ca.crt").await)).unwrap(),
    )
    .unwrap();
    let first = directory.search("fixture & owner", None, 1).await.unwrap();
    assert_eq!(first.items[0].identity.subject, "owner-a");
    assert_eq!(first.items[0].identity.issuer, issuer);
    assert_eq!(
        first.items[0].display_name.as_deref(),
        Some("Fixture Owner")
    );
    assert!(first.items[0].directory_verified);
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("never-return-this")
    );
    let cursor = first.next_cursor.as_deref().unwrap();
    assert!(matches!(
        directory.search("changed", Some(cursor), 1).await,
        Err(Error::Invalid)
    ));
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
    let provider = start(
        &secrets,
        Router::new().route(
            "/realms/agents/protocol/openid-connect/token",
            post(|| async { StatusCode::FORBIDDEN }),
        ),
        false,
    )
    .await;
    let directory = KeycloakDirectory::new(
        &format!("{}/realms/agents", provider.url),
        "directory-reader".into(),
        zeroize::Zeroizing::new("synthetic-secret".into()),
        http_client(Some(&secrets.read("ca.crt").await)).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        directory.search("", None, 50).await,
        Err(Error::Unavailable)
    ));
}
