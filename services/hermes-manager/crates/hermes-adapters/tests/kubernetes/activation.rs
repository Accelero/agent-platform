//! Actual constrained API + SQL policy activation and lost-reply recovery.
use hermes_adapters::{
    controller::Controller, execution_workloads, kubernetes::KubernetesDriver,
    materials::MaterialVault, ownership::OwnershipSigner, postgres::PgStore,
    session_crypto::Sealer,
};
use hermes_application::{
    Error, InstallationScope, OperationQueue, execution::*, resources::ResourceReconciler,
    terminal::activation::*,
};
use hermes_domain::{Identity, TemplateVersion};
use kube::{
    Api, Client, Config,
    api::{Patch, PatchParams},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use uuid::Uuid;

#[allow(clippy::too_many_arguments)] // Explicit real SQL, scoped transport and original owner fixture.
pub(super) async fn verify(
    pool: &PgPool,
    store: &PgStore,
    scope: &InstallationScope,
    config: Config,
    cluster: &str,
    controller: &Controller,
    assignment: Uuid,
    instance: Uuid,
) -> i64 {
    let resources = WorkerResources {
        cpu_millis: 100,
        memory_mib: 128,
        ephemeral_storage_mib: 128,
    };
    let definition = RuntimeDefinition {
        schema_version: 1,
        runtime: RuntimeRef {
            id: "activation-tools".into(),
            version: 1,
        },
        image: format!("ghcr.io/example/worker@sha256:{}", "a".repeat(64)),
        ssh_contract: "managed-ssh-v1".into(),
        uid: 1000,
        gid: 1000,
        service_account: "worker".into(),
        resources: resources.clone(),
        locale: "C.UTF-8".into(),
        egress: vec![],
        pull_secret_names: vec![],
    };
    let mutation = |key: &str, command| ExecutionMutation {
        scope: scope.clone(),
        actor: Identity {
            issuer: scope.issuer.clone(),
            subject: "fixture-admin".into(),
        },
        request_id: Uuid::new_v4(),
        idempotency_key: key.into(),
        command,
    };
    store
        .mutate_execution(mutation(
            "activation-runtime",
            ExecutionCommand::RegisterRuntime {
                definition: definition.clone(),
            },
        ))
        .await
        .unwrap();
    let accepted = store
        .mutate_execution(mutation(
            "activation-policy",
            ExecutionCommand::SetPolicy {
                assignment_id: assignment,
                expected_version: 1,
                policy: AssignmentExecution {
                    default_runtime_id: definition.runtime.id.clone(),
                    allowed: vec![AllowedRuntime {
                        runtime: definition.runtime,
                        resources,
                        mounts: vec![],
                        limits: Default::default(),
                    }],
                },
            },
        ))
        .await
        .unwrap();
    sqlx::query("UPDATE manager.operations SET next_attempt_at=clock_timestamp() WHERE instance_id=$1 AND state='Waiting'").bind(instance).execute(pool).await.unwrap();
    let operation = store
        .claim(scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let signer = OwnershipSigner::new(
        "fixture".into(),
        BTreeMap::from([("fixture".into(), vec![41; 32])]),
    )
    .unwrap();
    let client = Client::try_from(config.clone()).unwrap();
    let driver = KubernetesDriver::from_config(config, signer, cluster.into()).unwrap();
    let vault = MaterialVault::new(
        store.clone(),
        Sealer::new(
            "fixture".into(),
            BTreeMap::from([("fixture".into(), vec![42; 32])]),
        )
        .unwrap(),
    );
    let (material, bootstrap) = vault
        .native_bootstrap(
            scope,
            &operation,
            TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .await
        .unwrap();
    let desired = store
        .execution_policy(scope, assignment)
        .await
        .unwrap()
        .desired;
    let key = execution_workloads::authorized_keys(
        &desired,
        &bootstrap.ssh_public_key,
        material.context.id,
    )
    .unwrap();
    ResourceReconciler::new(store.clone(), driver.clone(), scope.clone())
        .ensure(
            &operation,
            "activation-keys",
            key.spec.clone(),
            &key.manifest,
        )
        .await
        .unwrap();
    let claim = store
        .claim_policy(scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let plan = store.activation_plan(scope, &claim).await.unwrap();
    let input = driver
        .inspect_policy_admission(&plan, scope.epoch)
        .await
        .unwrap();
    let prepared = store
        .prepare_policy_admission(scope, &claim, &input)
        .await
        .unwrap();
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("hermes.agent-platform.io", "v1alpha1", "InstancePolicy"),
        "instancepolicies",
    );
    let policies: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &desired.namespace, &resource);
    let before = policies.get("instance-policy").await.unwrap();
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(json!({"metadata":{"annotations":{"fixture-rv":"new"}}})),
        )
        .await
        .unwrap();
    assert!(matches!(
        driver.apply_policy_admission(&prepared).await,
        Err(Error::PreconditionFailed)
    ));
    let fresh = driver
        .inspect_policy_admission(&plan, scope.epoch)
        .await
        .unwrap();
    let renewed = store
        .prepare_policy_admission(scope, &claim, &fresh)
        .await
        .unwrap();
    let observed = driver.apply_policy_admission(&renewed).await.unwrap();
    assert_eq!(observed.policy_uid, input.policy_uid);
    // Simulate abrupt death after external success, before either readback is
    // committed. A real controller reclaims the SQL lease and recovers both RVs.
    sqlx::query("UPDATE manager.execution_policy_status SET lease_until=clock_timestamp()-interval '1 second' WHERE instance_id=$1").bind(instance).execute(pool).await.unwrap();
    controller.step().await.unwrap();
    let current = store.execution_policy(scope, assignment).await.unwrap();
    assert_eq!(current.desired_generation, 1);
    assert_eq!(current.applied_generation, 1);
    assert_eq!(current.state, "Applied");
    let pending:i64=sqlx::query_scalar("SELECT count(*) FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND verified_at IS NULL").bind(instance).fetch_one(pool).await.unwrap();
    assert_eq!(pending, 0);
    let original_rv:String=sqlx::query_scalar("SELECT input->>'resourceVersion' FROM manager.execution_policy_admission_inputs WHERE instance_id=$1 AND input_digest=$2").bind(instance).bind(input.digest().unwrap()).fetch_one(pool).await.unwrap();
    assert_eq!(original_rv, input.resource_version);
    let after = policies.get("instance-policy").await.unwrap();
    assert_eq!(after.metadata.uid, before.metadata.uid);
    assert_eq!(after.data["spec"]["native"], before.data["spec"]["native"]);
    for field in [
        "agent-platform.io/creation-proof",
        "agent-platform.io/applied-intent",
    ] {
        assert_eq!(
            after.metadata.annotations.as_ref().unwrap().get(field),
            before.metadata.annotations.as_ref().unwrap().get(field)
        );
    }
    // A same-generation mutation cannot masquerade as the approved readback.
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"worker":{"digest":"f".repeat(64)}}})),
        )
        .await
        .unwrap();
    assert!(matches!(
        driver.inspect_policy_admission(&plan, scope.epoch).await,
        Err(Error::OwnershipConflict)
    ));
    policies
        .patch(
            "instance-policy",
            &PatchParams::default(),
            &Patch::Merge(json!({"spec":{"worker":plan.worker}})),
        )
        .await
        .unwrap();
    let ready: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.execution_worker_ready_receipts")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(ready, 0);
    accepted.assignment_version.unwrap()
}
