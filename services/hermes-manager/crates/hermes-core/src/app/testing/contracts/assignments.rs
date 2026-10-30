//! Assignment cases: pins, volumes, mounts, generations and owner isolation.

use std::marker::PhantomData;

use serde_json::json;
use uuid::Uuid;

use super::super::{Inspect, identity, installation, join};
use super::access::{admin, bootstrap, envelope, key};
use super::catalog::{backend_definition, draft, published_catalog};
use crate::app::assignments::{
    AssignmentChanges, AssignmentReceipt, CreateAssignment, RetryInstance, UpdateAssignment,
};
use crate::app::catalog::{VersionReceipt, Withdraw};
use crate::app::error::AppError;
use crate::app::ports::Store;
use crate::domain::ErrorKind;
use crate::domain::assignments::{Access, AssignmentSpec};
use crate::domain::backends::BackendDefinition;
use crate::domain::templates::TemplateDefinition;

/// Bob's example assignment from the specification.
pub fn bob_spec() -> AssignmentSpec {
    serde_json::from_value(json!({
        "access": "enabled",
        "template": {"id": "personal-default", "version": 1},
        "volumes": {
            "workspace": {"source": "nvme", "sizeGib": 20},
            "home": {"source": "nas", "path": "bob"},
        },
        "terminal": {"default": "python-tools", "allowed": {"python-tools": 1}},
    }))
    .expect("fixture assignment")
}

pub(super) async fn create<S: Store + Clone>(
    store: &S,
    subject: &str,
    spec: AssignmentSpec,
) -> Result<AssignmentReceipt, AppError> {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &CreateAssignment {
                identity: identity(subject),
                spec,
            },
        )
        .await
}

async fn update<S: Store + Clone>(
    store: &S,
    receipt: &AssignmentReceipt,
    changes: AssignmentChanges,
) -> Result<AssignmentReceipt, AppError> {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &UpdateAssignment {
                assignment_id: receipt.assignment_id,
                expected_version: receipt.version,
                changes,
            },
        )
        .await
}

fn problems(error: AppError) -> Vec<(String, &'static str)> {
    assert_eq!(error.code, "invalid_configuration", "{error:?}");
    error
        .problems
        .into_iter()
        .map(|p| (p.pointer, p.code))
        .collect()
}

pub async fn bob_gets_a_shared_workspace_and_his_network_home<S: Store + Inspect + Clone>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let receipt = create(&store, "bob", bob_spec()).await.unwrap();
    assert_eq!(receipt.version, 1);
    assert_eq!(
        store.instance_generations(receipt.instance_id).await,
        (1, 1)
    );
    assert_eq!(
        store.instance_namespace(receipt.instance_id).await,
        installation().instance_namespace(receipt.instance_id)
    );
    assert_eq!(
        store.audit_actions().await.last().map(String::as_str),
        Some("assignment.create")
    );
    let again = create(&store, "bob", bob_spec()).await.unwrap_err();
    assert_eq!(
        (again.kind, again.code),
        (ErrorKind::Conflict, "assignment_exists")
    );
}

pub async fn concurrent_enables_for_one_identity_commission_one_instance<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let command = CreateAssignment {
        identity: identity("bob"),
        spec: bob_spec(),
    };
    let attempt = |name: &'static str| {
        let (store, command) = (store.clone(), command.clone());
        async move {
            envelope(&store)
                .execute(&admin("alice"), Uuid::new_v4(), &key(name), &command)
                .await
        }
    };
    let (first, second) = join(attempt("first"), attempt("second")).await;
    let (winner, receipt, refused) = match (first, second) {
        (Ok(receipt), Err(refused)) => ("first", receipt, refused),
        (Err(refused), Ok(receipt)) => ("second", receipt, refused),
        other => panic!("exactly one concurrent enable must win: {other:?}"),
    };
    assert_eq!(
        (refused.kind, refused.code),
        (ErrorKind::Conflict, "assignment_exists")
    );
    // A retry after a lost response replays the winner's receipt.
    assert_eq!(attempt(winner).await.unwrap(), receipt);
    let created = store
        .audit_actions()
        .await
        .into_iter()
        .filter(|action| action == "assignment.create")
        .count();
    assert_eq!(created, 1, "one assignment and one instance");
    assert_eq!(
        store.instance_generations(receipt.instance_id).await,
        (1, 1)
    );
}

pub async fn missing_volumes_default_from_the_template<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let mut without_terminal = bob_spec();
    without_terminal.volumes.clear();
    without_terminal.terminal = Default::default();
    create(&store, "carol", without_terminal).await.unwrap();
    // The template's default `workspace` also fills the backend's required slot;
    // the optional `home` slot stays unbound.
    let mut with_terminal = bob_spec();
    with_terminal.volumes.clear();
    create(&store, "dave", with_terminal).await.unwrap();
}

pub async fn only_published_versions_can_be_newly_selected<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    let (template, _) = published_catalog(&store).await;
    let draft_template = draft(
        &store,
        "personal-default",
        super::catalog::template_definition(),
    )
    .await;
    let mut on_draft = bob_spec();
    on_draft.template.version = draft_template.version;
    assert_eq!(
        create(&store, "bob", on_draft).await.unwrap_err().code,
        "template_unavailable"
    );

    let bob = create(&store, "bob", bob_spec()).await.unwrap();
    withdraw_template(&store, &template).await;
    let mut late = bob_spec();
    late.volumes.get_mut("home").unwrap().path = Some("erin".into());
    assert_eq!(
        create(&store, "erin", late).await.unwrap_err().code,
        "template_unavailable"
    );
    let suspended = update(
        &store,
        &bob,
        AssignmentChanges {
            access: Some(Access::Suspended),
            ..Default::default()
        },
    )
    .await
    .expect("an existing pin stays usable after withdrawal");
    assert_eq!(suspended.version, 2);

    let unpublished = draft(&store, "gpu-tools", backend_definition()).await;
    let mut terminal = bob_spec().terminal;
    terminal
        .allowed
        .insert("gpu-tools".into(), unpublished.version);
    let refused = update(
        &store,
        &suspended,
        AssignmentChanges {
            terminal: Some(terminal),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        problems(refused),
        [(
            "/terminal/allowed/gpu-tools".to_owned(),
            "backend_unavailable"
        )]
    );
}

async fn withdraw_template<S: Store + Clone>(store: &S, receipt: &VersionReceipt) {
    envelope(store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key(&Uuid::new_v4().to_string()),
            &Withdraw::<TemplateDefinition> {
                id: receipt.id.clone(),
                version: receipt.version,
                expected_revision: receipt.revision,
                kind: PhantomData,
            },
        )
        .await
        .expect("withdraw");
}

pub async fn owners_share_static_paths_even_concurrently<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let shared = |subject: &'static str, path: &'static str, read_only: bool| {
        let mut spec = bob_spec();
        let home = spec.volumes.get_mut("home").unwrap();
        home.path = Some(path.into());
        home.read_only = read_only;
        let store = store.clone();
        async move { create(&store, subject, spec).await.unwrap() }
    };
    let (first, second) = join(
        shared("bob", "shared", false),
        shared("carol", "shared", true),
    )
    .await;
    let (third, fourth) = join(
        shared("dave", "shared/inner", true),
        shared("erin", "shared", false),
    )
    .await;
    for (left, right) in [(first, second), (third, fourth)] {
        let change = |read_only| {
            let mut volumes = bob_spec().volumes;
            let home = volumes.get_mut("home").unwrap();
            home.path = Some("shared/inner".into());
            home.read_only = read_only;
            AssignmentChanges {
                volumes: Some(volumes),
                ..Default::default()
            }
        };
        let (left, right) = join(
            update(&store, &left, change(false)),
            update(&store, &right, change(true)),
        )
        .await;
        assert_eq!(left.unwrap().version, 2);
        assert_eq!(right.unwrap().version, 2);
    }
    for (subject, path) in [("mallory", "bob"), ("frank", "bob/inner")] {
        let mut shared = bob_spec();
        let mut scratch = shared.volumes["home"].clone();
        scratch.path = Some(path.into());
        scratch.read_only = true;
        shared.volumes.insert("scratch".into(), scratch);
        create(&store, subject, shared).await.unwrap();
    }
}

pub async fn updates_need_the_current_version_and_advance_generations<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let created = create(&store, "bob", bob_spec()).await.unwrap();
    let suspended = update(
        &store,
        &created,
        AssignmentChanges {
            access: Some(Access::Suspended),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        store.instance_generations(created.instance_id).await,
        (2, 1)
    );
    let mut terminal = bob_spec().terminal;
    terminal.default = None;
    let changed = update(
        &store,
        &suspended,
        AssignmentChanges {
            terminal: Some(terminal),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        store.instance_generations(created.instance_id).await,
        (3, 2)
    );

    let stale = update(&store, &suspended, AssignmentChanges::default())
        .await
        .unwrap_err();
    assert_eq!(
        (stale.kind, stale.code),
        (ErrorKind::Precondition, "stale_version")
    );
    let mut volumes = bob_spec().volumes;
    volumes.get_mut("workspace").unwrap().size_gib = Some(5);
    let shrink = update(
        &store,
        &changed,
        AssignmentChanges {
            volumes: Some(volumes),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        problems(shrink),
        [(
            "/volumes/workspace".to_owned(),
            "storage_migration_required"
        )]
    );
    let retried = envelope(&store)
        .execute(
            &admin("alice"),
            Uuid::new_v4(),
            &key("retry"),
            &RetryInstance {
                assignment_id: changed.assignment_id,
                expected_version: changed.version,
            },
        )
        .await
        .unwrap();
    assert_eq!(retried.version, changed.version);
    assert_eq!(
        store.instance_generations(created.instance_id).await,
        (4, 2)
    );
}

pub async fn mount_rules_are_enforced_when_saving<S: Store + Inspect + Clone>(store: S) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let mut moved = backend_definition();
    moved.mounts.get_mut("workspace").unwrap().mount_path = "/home/worker/workspace".into();
    let moved = draft(&store, "moved-tools", moved).await;
    super::catalog::publish::<S, BackendDefinition>(&store, &moved)
        .await
        .unwrap();
    let mut spec = bob_spec();
    spec.terminal.allowed.insert("moved-tools".into(), 1);
    spec.volumes.insert(
        "scratch".into(),
        serde_json::from_value(json!({"source": "nas", "path": "bob"})).unwrap(),
    );
    spec.secrets = serde_json::from_value(json!([{"name": "missing", "env": "API_KEY"}])).unwrap();
    let refused = create(&store, "bob", spec).await.unwrap_err();
    assert_eq!(
        problems(refused),
        [
            (
                "/terminal/allowed/moved-tools/workspace".to_owned(),
                "mount_path_mismatch"
            ),
            ("/secrets/0/name".to_owned(), "unknown_secret"),
        ]
    );
}
