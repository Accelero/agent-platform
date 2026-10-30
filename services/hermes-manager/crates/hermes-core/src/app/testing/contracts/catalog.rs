//! Catalog cases: drafts, publication, withdrawal and secret references.

use std::marker::PhantomData;

use serde_json::json;
use uuid::Uuid;

use super::super::Inspect;
use super::access::{admin, bootstrap, envelope, key};
use crate::app::catalog::{CreateDraft, Definition, EditDraft, Publish, VersionReceipt, Withdraw};
use crate::app::error::AppError;
use crate::app::ports::Store;
use crate::app::secrets::WriteSecret;
use crate::domain::ErrorKind;
use crate::domain::backends::BackendDefinition;
use crate::domain::secrets::SecretKind;
use crate::domain::templates::TemplateDefinition;

/// A template valid for the fixture settings.
pub fn template_definition() -> TemplateDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "image": format!("ghcr.io/accelero/agent-platform/hermes@sha256:{}", "a".repeat(64)),
        "resources": {"cpuMillis": 1000, "memoryMib": 2048},
        "home": {"source": "nvme", "sizeGib": 5},
        "mounts": {"workspace": {"mountPath": "/workspace"}},
        "defaultVolumes": {"workspace": {"source": "nvme", "sizeGib": 10}},
        "adminConfig": {"model": {"provider": "custom"}},
    }))
    .expect("fixture template")
}

/// A terminal backend valid for the fixture settings.
pub fn backend_definition() -> BackendDefinition {
    serde_json::from_value(json!({
        "image": format!("ghcr.io/accelero/agent-platform/agent-runtime@sha256:{}", "b".repeat(64)),
        "sshContract": "managed-ssh-v2",
        "uid": 1000,
        "gid": 1000,
        "resources": {"cpuMillis": 2000, "memoryMib": 4096, "ephemeralGib": 10},
        "limits": {"idleSeconds": 900, "maxLeaseSeconds": 3600, "maxEnvironments": 4},
        "mounts": {
            "workspace": {"mountPath": "/workspace"},
            "home": {"mountPath": "/home/worker/home", "optional": true},
        },
    }))
    .expect("fixture backend")
}

pub(super) async fn draft<S: Store + Clone, D: Definition>(
    store: &S,
    id: &str,
    definition: D,
) -> VersionReceipt {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &CreateDraft {
                id: id.into(),
                definition,
            },
        )
        .await
        .expect("draft")
}

pub(super) async fn publish<S: Store + Clone, D: Definition>(
    store: &S,
    receipt: &VersionReceipt,
) -> Result<VersionReceipt, AppError> {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &Publish::<D> {
                id: receipt.id.clone(),
                version: receipt.version,
                expected_revision: receipt.revision,
                kind: PhantomData,
            },
        )
        .await
}

/// Publishes the fixture template and backend; returns their receipts.
pub(super) async fn published_catalog<S: Store + Clone>(
    store: &S,
) -> (VersionReceipt, VersionReceipt) {
    published_catalog_with(store, backend_definition()).await
}

/// The same, with `python-tools` defined as given.
pub(super) async fn published_catalog_with<S: Store + Clone>(
    store: &S,
    definition: BackendDefinition,
) -> (VersionReceipt, VersionReceipt) {
    let template = draft(store, "personal-default", template_definition()).await;
    let template = publish::<S, TemplateDefinition>(store, &template)
        .await
        .expect("publish template");
    let backend = draft(store, "python-tools", definition).await;
    let backend = publish::<S, BackendDefinition>(store, &backend)
        .await
        .expect("publish backend");
    (template, backend)
}

pub(super) async fn secret<S: Store + Clone>(store: &S, name: &str, kind: SecretKind) {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &WriteSecret {
                name: name.into(),
                kind,
                sealed: b"sealed".to_vec(),
            },
        )
        .await
        .expect("secret");
}

pub(super) fn problems(error: AppError) -> Vec<(String, &'static str)> {
    assert_eq!(
        (error.kind, error.code),
        (ErrorKind::Invalid, "invalid_configuration")
    );
    error
        .problems
        .into_iter()
        .map(|p| (p.pointer, p.code))
        .collect()
}

pub async fn drafts_are_versioned_and_publication_validates_settings<S: Store + Inspect + Clone>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    let mut invalid = template_definition();
    invalid.home.source = "elsewhere".into();
    let first = draft(&store, "personal-default", invalid).await;
    assert_eq!((first.version, first.revision), (1, 1));
    let refused = publish::<S, TemplateDefinition>(&store, &first)
        .await
        .unwrap_err();
    assert_eq!(
        problems(refused),
        [("/home/source".to_owned(), "unknown_storage_source")]
    );

    let edited = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("edit"),
            &EditDraft {
                id: "personal-default".into(),
                version: 1,
                expected_revision: 1,
                definition: template_definition(),
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.revision, 2);
    let published = publish::<S, TemplateDefinition>(&store, &edited)
        .await
        .unwrap();
    assert_eq!(published.revision, 3);

    let frozen = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("edit-published"),
            &EditDraft {
                id: "personal-default".into(),
                version: 1,
                expected_revision: 3,
                definition: template_definition(),
            },
        )
        .await;
    assert_eq!(frozen.unwrap_err().code, "version_not_draft");
    let next = draft(&store, "personal-default", template_definition()).await;
    assert_eq!(next.version, 2);

    let withdrawn = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("withdraw"),
            &Withdraw::<TemplateDefinition> {
                id: "personal-default".into(),
                version: 1,
                expected_revision: 3,
                kind: PhantomData,
            },
        )
        .await
        .unwrap();
    assert_eq!(withdrawn.revision, 4);
    let actions = store.audit_actions().await;
    assert_eq!(
        &actions[1..],
        [
            "template.create",
            "template.edit",
            "template.publish",
            "template.create",
            "template.withdraw"
        ]
    );
}

pub async fn stale_revisions_and_unknown_versions_are_refused<S: Store + Inspect + Clone>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    let first = draft(&store, "python-tools", backend_definition()).await;
    let stale = VersionReceipt {
        revision: first.revision + 1,
        ..first.clone()
    };
    let refused = publish::<S, BackendDefinition>(&store, &stale)
        .await
        .unwrap_err();
    assert_eq!(
        (refused.kind, refused.code),
        (ErrorKind::Precondition, "stale_version")
    );
    let missing = VersionReceipt {
        version: 9,
        ..first.clone()
    };
    assert_eq!(
        publish::<S, BackendDefinition>(&store, &missing)
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    let mut other = backend_definition();
    other.uid = 2000;
    other.gid = 3000;
    let other = draft(&store, "other-user-tools", other).await;
    publish::<S, BackendDefinition>(&store, &other)
        .await
        .unwrap();
    let mut root = backend_definition();
    root.uid = 0;
    let rooted = draft(&store, "root-tools", root).await;
    let refused = publish::<S, BackendDefinition>(&store, &rooted)
        .await
        .unwrap_err();
    assert_eq!(problems(refused), [("/uid".to_owned(), "out_of_range")]);
}

pub async fn publication_requires_referenced_secrets_of_the_right_kind<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    let mut with_secret = template_definition();
    with_secret.secrets =
        serde_json::from_value(json!([{"name": "llm-key", "env": "OPENAI_API_KEY"}])).unwrap();
    let pending = draft(&store, "with-secret", with_secret).await;
    let refused = publish::<S, TemplateDefinition>(&store, &pending)
        .await
        .unwrap_err();
    assert_eq!(
        problems(refused),
        [("/secrets/0/name".to_owned(), "unknown_secret")]
    );
    secret(&store, "llm-key", SecretKind::File).await;
    let refused = publish::<S, TemplateDefinition>(&store, &pending)
        .await
        .unwrap_err();
    assert_eq!(
        problems(refused),
        [("/secrets/0/name".to_owned(), "wrong_secret_kind")]
    );
    secret(&store, "llm-env", SecretKind::Env).await;
    let mut fixed = template_definition();
    fixed.secrets =
        serde_json::from_value(json!([{"name": "llm-env", "env": "OPENAI_API_KEY"}])).unwrap();
    let fixed = draft(&store, "with-secret", fixed).await;
    publish::<S, TemplateDefinition>(&store, &fixed)
        .await
        .unwrap();
}
