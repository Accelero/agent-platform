//! Secret cases: sealed versions, fixed kinds, references and restarts.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::super::Inspect;
use super::access::{admin, bootstrap, envelope, key};
use super::assignments::bob_spec;
use super::catalog::{draft, publish, published_catalog, secret, template_definition};
use crate::app::assignments::CreateAssignment;
use crate::app::error::AppError;
use crate::app::ports::Store;
use crate::app::secrets::{self, DeleteSecret, SecretSealer};
use crate::domain::ErrorKind;
use crate::domain::secrets::SecretKind;
use crate::domain::templates::TemplateDefinition;

/// Marks values as sealed without encrypting them.
struct Marking;

impl SecretSealer for Marking {
    fn seal(&self, _: &str, _: SecretKind, value: &[u8]) -> Result<Vec<u8>, AppError> {
        Ok([b"sealed:".as_slice(), value].concat())
    }
}

async fn delete<S: Store + Clone>(store: &S, name: &str, version: i64) -> Result<(), AppError> {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &DeleteSecret {
                name: name.into(),
                expected_version: version,
            },
        )
        .await
        .map(|_| ())
}

pub async fn secrets_are_sealed_versions_with_a_fixed_kind<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    let sealer: Arc<dyn SecretSealer> = Arc::new(Marking);
    let envelope = envelope(&store);
    let write = |value: &'static [u8], kind: SecretKind| {
        let (envelope, sealer) = (&envelope, &sealer);
        async move {
            let key = key(&Uuid::new_v4().to_string());
            secrets::write(
                envelope,
                sealer,
                &admin("alice"),
                Uuid::new_v4(),
                &key,
                secrets::PlainSecret {
                    name: "llm-key",
                    kind,
                    value,
                },
            )
            .await
        }
    };
    assert_eq!(write(b"sk-one", SecretKind::Env).await.unwrap().version, 1);
    assert_eq!(write(b"sk-two", SecretKind::Env).await.unwrap().version, 2);
    assert_eq!(
        store.secret_versions("llm-key").await,
        [
            (1, b"sealed:sk-one".to_vec()),
            (2, b"sealed:sk-two".to_vec())
        ]
    );
    assert_eq!(
        write(b"x", SecretKind::File).await.unwrap_err().code,
        "invalid_secret_value"
    );
    assert_eq!(
        write(b"", SecretKind::Env).await.unwrap_err().code,
        "invalid_secret_value"
    );
    let stale = delete(&store, "llm-key", 1).await.unwrap_err();
    assert_eq!(
        (stale.kind, stale.code),
        (ErrorKind::Precondition, "stale_version")
    );
    delete(&store, "llm-key", 2).await.unwrap();
    assert!(store.secret_versions("llm-key").await.is_empty());
    assert_eq!(
        delete(&store, "llm-key", 2).await.unwrap_err().code,
        "not_found"
    );
}

pub async fn referenced_secrets_cannot_be_deleted_and_new_versions_restart_users<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    secret(&store, "llm-key", SecretKind::Env).await;
    secret(&store, "ca-bundle", SecretKind::File).await;
    let mut with_secret = template_definition();
    with_secret.secrets =
        serde_json::from_value(json!([{"name": "ca-bundle", "file": "ca"}])).unwrap();
    let template = draft(&store, "with-secret", with_secret).await;
    publish::<S, TemplateDefinition>(&store, &template)
        .await
        .unwrap();

    let mut spec = bob_spec();
    spec.secrets =
        serde_json::from_value(json!([{"name": "llm-key", "env": "OPENAI_API_KEY"}])).unwrap();
    let bob = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("bob"),
            &CreateAssignment {
                identity: super::super::identity("bob"),
                spec,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        delete(&store, "llm-key", 1).await.unwrap_err().code,
        "secret_in_use"
    );
    assert_eq!(
        delete(&store, "ca-bundle", 1).await.unwrap_err().code,
        "secret_in_use"
    );

    let before = store.instance_generations(bob.instance_id).await;
    secret(&store, "llm-key", SecretKind::Env).await;
    let after = store.instance_generations(bob.instance_id).await;
    assert_eq!((after.0, after.1), (before.0 + 1, before.1));
    secret(&store, "unused", SecretKind::Env).await;
    assert_eq!(store.instance_generations(bob.instance_id).await, after);
}
