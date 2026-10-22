//! Exercise manager-owned publication and explicit apply against real native Pods.
use super::*;
use hermes_adapters::postgres::{
    authoring::{DraftChange, DraftInput},
    rollout::{BatchMember, TemplateSelection},
};
use hermes_application::{AdminPolicy, AssignmentCommand, Assignments, VerifiedPrincipal};
use hermes_domain::{Access, Identity, TemplateVersion};
use std::collections::BTreeSet;

async fn publish(
    store: &PgStore,
    scope: &InstallationScope,
    actor: &VerifiedPrincipal,
    value: Value,
    key: &str,
) -> i64 {
    let input = DraftInput {
        template_id: "dashboard-native".into(),
        definition: serde_json::from_value(value).unwrap(),
    };
    let created = store
        .dashboard_save_draft(
            scope,
            actor,
            &format!("{key}-draft"),
            DraftChange {
                id: None,
                expected: None,
                input: input.clone(),
                publish: false,
            },
        )
        .await
        .unwrap();
    let result = store
        .dashboard_save_draft(
            scope,
            actor,
            &format!("{key}-publish"),
            DraftChange {
                id: Some(serde_json::from_value(created["draft"]["id"].clone()).unwrap()),
                expected: Some(1),
                input,
                publish: true,
            },
        )
        .await
        .unwrap();
    result["draft"]["publishedVersion"].as_i64().unwrap()
}
pub(super) async fn verify(
    pool: &PgPool,
    store: &PgStore,
    scope: &InstallationScope,
    controller: &Controller,
    progress: &Api<ConfigMap>,
) {
    let actor = VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "dashboard-administrator".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::new(),
    };
    let mut definition = json!({"schemaVersion":1,"image":std::env::var("HERMES_NATIVE_FIXTURE_IMAGE").unwrap(),"storageClass":"local-path","homeGib":1,"workspaceGib":1,"cpuMillis":500,"memoryMib":1024,"adminConfig":{"agent":{"max_turns":12}}});
    assert_eq!(
        publish(store, scope, &actor, definition.clone(), "initial").await,
        1
    );
    let assignments = Assignments::new(
        store.clone(),
        scope.clone(),
        AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: "manager-api".into(),
            management_client: String::new(),
            administrator_role: String::new(),
        },
    )
    .unwrap();
    let accepted = assignments
        .execute(
            &actor,
            Uuid::new_v4(),
            "dashboard-owner",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "dashboard-owner".into(),
                },
                access: Access::Enabled,
                template: TemplateVersion {
                    id: "dashboard-native".into(),
                    version: 1,
                },
            },
        )
        .await
        .unwrap();
    drive(controller, pool, 3, 0).await;
    let namespace: String =
        sqlx::query_scalar("SELECT namespace FROM manager.instances WHERE id=$1")
            .bind(accepted.instance_id)
            .fetch_one(pool)
            .await
            .unwrap();
    stage(progress, "dashboard-ready", &namespace, &namespace).await;
    for (version, name, config) in [
        (2, "dashboard-applied", json!({"agent":{"max_turns":24}})),
        (3, "dashboard-removed", json!({})),
    ] {
        definition["adminConfig"] = config;
        assert_eq!(
            publish(store, scope, &actor, definition.clone(), name).await,
            version
        );
        let expected: i64 =
            sqlx::query_scalar("SELECT version FROM manager.assignments WHERE id=$1")
                .bind(accepted.assignment_id)
                .fetch_one(pool)
                .await
                .unwrap();
        store
            .dashboard_apply_template(
                scope,
                &actor,
                name,
                &BatchMember {
                    assignment_id: accepted.assignment_id,
                    expected_version: expected,
                },
                &TemplateSelection {
                    template_id: "dashboard-native".into(),
                    template_version: version,
                },
            )
            .await
            .unwrap();
        drive(controller, pool, 3, 0).await;
        let current: (Uuid, String) =
            sqlx::query_as("SELECT id,namespace FROM manager.instances WHERE assignment_id=$1")
                .bind(accepted.assignment_id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(current, (accepted.instance_id, namespace.clone()));
        let observation = store
            .dashboard_template_observation(scope, accepted.assignment_id)
            .await
            .unwrap();
        assert_eq!(observation["desiredTemplate"]["version"], version);
        assert_eq!(observation["observedTemplate"]["version"], version);
        stage(progress, name, &namespace, &namespace).await;
    }
}
