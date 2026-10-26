//! Access-token verification against a fake realm over real TLS.

use std::collections::BTreeSet;

use hermes_identity::tokens::{AccessTokenVerifier, TokenPolicy};
use hermes_identity::{AuthError, http_client};
use hermes_testkit::Secrets;
use hermes_testkit::provider::FakeProvider;
use jsonwebtoken::{Algorithm, Header, encode};
use serde_json::{Value, json};

struct Realm {
    _secrets: Secrets,
    provider: FakeProvider,
    verifier: AccessTokenVerifier,
}

fn policy(provider: &FakeProvider) -> TokenPolicy {
    TokenPolicy {
        issuer: provider.issuer.clone(),
        jwks_url: provider.jwks_url(),
        audience: "manager-api".into(),
        authorized_clients: BTreeSet::from(["manager-admin".into()]),
        algorithms: vec![Algorithm::RS256],
        maximum_age_seconds: None,
    }
}

async fn realm() -> Realm {
    let secrets = Secrets::new().await;
    let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
    let client = http_client(Some(&secrets.read("ca.crt"))).unwrap();
    let verifier = AccessTokenVerifier::new(policy(&provider), client).unwrap();
    Realm {
        _secrets: secrets,
        provider,
        verifier,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn valid_access_token_yields_the_exact_identity() {
    let realm = realm().await;
    let verified = realm
        .verifier
        .verify(&realm.provider.access_token("owner-a"))
        .await
        .unwrap();
    assert_eq!(verified.identity().subject, "owner-a");
    assert_eq!(verified.identity().issuer, realm.provider.issuer);
    assert_eq!(verified.provider_session(), "provider-session-one");
    assert_eq!(verified.authorized_client(), "manager-admin");
}

#[tokio::test]
async fn tokens_with_wrong_claims_are_rejected() {
    let realm = realm().await;
    let changes: Vec<(&str, Value)> = vec![
        ("aud", json!("route-api")),
        ("azp", json!("external-panel")),
        ("typ", json!("ID")),
        ("sid", json!("")),
        ("gty", json!("client_credentials")),
        ("iss", json!("https://other.example.org/realms/agents")),
        ("exp", json!(now() - 1)),
        ("iat", json!(now() + 60)),
        ("sub", json!("")),
    ];
    for (claim, value) in changes {
        let mut claims = realm.provider.access_claims("owner-a");
        claims[claim] = value;
        let result = realm.verifier.verify(&realm.provider.sign(&claims)).await;
        assert_eq!(result.err(), Some(AuthError::Evidence), "{claim}");
    }
    let mut missing_sid = realm.provider.access_claims("owner-a");
    missing_sid.as_object_mut().unwrap().remove("sid");
    let result = realm
        .verifier
        .verify(&realm.provider.sign(&missing_sid))
        .await;
    assert_eq!(result.err(), Some(AuthError::Evidence));
}

#[tokio::test]
async fn tokens_with_unpinned_keys_or_headers_are_rejected() {
    let realm = realm().await;
    let claims = realm.provider.access_claims("owner-a");
    let mut unknown_kid = Header::new(Algorithm::RS256);
    unknown_kid.kid = Some("other".into());
    let mut remote_keys = Header::new(Algorithm::RS256);
    remote_keys.kid = Some("fixture".into());
    remote_keys.jku = Some("https://attacker.example.org/jwks".into());
    let mut no_kid = Header::new(Algorithm::RS256);
    no_kid.kid = None;
    for header in [unknown_kid, remote_keys, no_kid] {
        let token = encode(&header, &claims, realm.provider.signer()).unwrap();
        assert_eq!(
            realm.verifier.verify(&token).await.err(),
            Some(AuthError::Evidence)
        );
    }
    for token in ["", "not-a-token", &"a".repeat(17 * 1024)] {
        assert_eq!(
            realm.verifier.verify(token).await.err(),
            Some(AuthError::Evidence)
        );
    }
}

#[tokio::test]
async fn maximum_age_rejects_old_evidence() {
    let realm = realm().await;
    let mut policy = policy(&realm.provider);
    policy.maximum_age_seconds = Some(120);
    let client = http_client(Some(&realm._secrets.read("ca.crt"))).unwrap();
    let strict = AccessTokenVerifier::new(policy, client).unwrap();
    let mut claims = realm.provider.access_claims("owner-a");
    claims["iat"] = json!(now() - 121);
    assert_eq!(
        strict.verify(&realm.provider.sign(&claims)).await.err(),
        Some(AuthError::Evidence)
    );
    claims["iat"] = json!(now() - 60);
    assert!(strict.verify(&realm.provider.sign(&claims)).await.is_ok());
}

#[tokio::test]
async fn unreachable_jwks_is_unavailable_never_a_pass() {
    let secrets = Secrets::new().await;
    let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
    let token = provider.access_token("owner-a");
    let mut unreachable = policy(&provider);
    unreachable.jwks_url = format!("{}/missing-jwks", provider.issuer);
    let client = http_client(Some(&secrets.read("ca.crt"))).unwrap();
    let verifier = AccessTokenVerifier::new(unreachable, client).unwrap();
    assert_eq!(
        verifier.verify(&token).await.err(),
        Some(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn policies_pin_the_jwks_origin_and_algorithms() {
    let secrets = Secrets::new().await;
    let provider = FakeProvider::start(&secrets, "manager-admin", "manager-api").await;
    let client = http_client(Some(&secrets.read("ca.crt"))).unwrap();
    let mut foreign_keys = policy(&provider);
    foreign_keys.jwks_url = "https://keys.example.org/jwks".into();
    let mut weak = policy(&provider);
    weak.algorithms = vec![Algorithm::HS256];
    let mut no_clients = policy(&provider);
    no_clients.authorized_clients.clear();
    let mut long_age = policy(&provider);
    long_age.maximum_age_seconds = Some(301);
    for policy in [foreign_keys, weak, no_clients, long_age] {
        assert_eq!(
            AccessTokenVerifier::new(policy, client.clone()).err(),
            Some(AuthError::Configuration)
        );
    }
}
