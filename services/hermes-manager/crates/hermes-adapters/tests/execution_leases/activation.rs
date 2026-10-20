//! Real SQL activation fencing, readback and immutable action recovery. These
//! observations are explicit trusted driver fixtures; live API proof is separate.
use super::*;
use hermes_application::{
    execution::{ExecutionRepository, PolicyClaim},
    resources::{ResourceObservation, ResourceRepository},
    terminal::activation::*,
};
use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention};

async fn activation_fixture(
    pool: &PgPool,
) -> (
    PgStore,
    InstallationScope,
    ExecutionClient,
    ResolvedPolicy,
    PolicyClaim,
) {
    let (store, scope, client, policy) = prepared(pool, false).await;
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE instance_id=$1").bind(client.instance_id).execute(pool).await.unwrap();
    let operation = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let material:Uuid=sqlx::query_scalar("SELECT id FROM manager.bootstrap_materials WHERE instance_id=$1 AND purpose='native-bootstrap-v1'").bind(client.instance_id).fetch_one(pool).await.unwrap();
    for (kind, name, purpose) in [
        (
            ResourceKind::InstancePolicy,
            "instance-policy",
            "instance-policy",
        ),
        (ResourceKind::ServiceAccount, "worker", "worker-account"),
        (
            ResourceKind::Secret,
            "worker-authorized-keys",
            "worker-keys",
        ),
    ] {
        let spec = ResourceSpec {
            kind,
            namespace: Some(policy.namespace.clone()),
            namespace_uid: Some(policy.namespace_uid.clone()),
            name: name.into(),
            manifest_digest: "b".repeat(64),
            purpose: purpose.into(),
            retention: Retention::Retain,
            encrypted_material_ref: (kind == ResourceKind::Secret).then_some(material),
        };
        store
            .prepare_resource(&scope, &operation, purpose, spec.clone())
            .await
            .unwrap();
        store
            .bind_resource(
                &scope,
                &operation,
                purpose,
                ResourceObservation {
                    uid: Uuid::new_v4().to_string(),
                    resource_version: "1".into(),
                    manifest_digest: spec.manifest_digest,
                },
            )
            .await
            .unwrap();
    }
    let claim = store
        .claim_policy(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    (store, scope, client, policy, claim)
}
fn input(plan: &ActivationPlan, claim: &PolicyClaim, rv: &str) -> PolicyAdmissionInput {
    PolicyAdmissionInput {
        installation_id: plan.policy.installation_id,
        instance_id: plan.policy.instance_id,
        generation: plan.policy.generation,
        epoch: claim.epoch,
        assignment_generation: plan.assignment_generation,
        namespace: plan.policy.namespace.clone(),
        namespace_uid: plan.policy.namespace_uid.clone(),
        resource_id: plan.parameters.creation.id,
        policy_uid: plan.parameters.uid.clone().unwrap(),
        resource_version: rv.into(),
        policy_digest: claim.digest.clone(),
        rendering_digest: plan.rendering_digest.clone(),
    }
}
fn observation(input: &PolicyAdmissionInput, rv: &str) -> PolicyAdmissionObservation {
    PolicyAdmissionObservation {
        input_digest: input.digest().unwrap(),
        policy_uid: input.policy_uid.clone(),
        resource_version: rv.into(),
        namespace_uid: input.namespace_uid.clone(),
        rendering_digest: input.rendering_digest.clone(),
    }
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn activation_requires_every_original_input_observed_and_atomic_applied_receipt(
    pool: PgPool,
) {
    let (store, scope, client, policy, claim) = activation_fixture(&pool).await;
    let plan = store.activation_plan(&scope, &claim).await.unwrap();
    let first = input(&plan, &claim, "1");
    store
        .prepare_policy_admission(&scope, &claim, &first)
        .await
        .unwrap();
    assert_eq!(
        store
            .activate_execution_policy(&scope, &claim, &first.digest().unwrap())
            .await,
        Err(Error::PreconditionFailed)
    );
    let next = input(&plan, &claim, "2");
    store
        .prepare_policy_admission(&scope, &claim, &next)
        .await
        .unwrap();
    let mut forged = observation(&next, "3");
    forged.policy_uid = "foreign".into();
    assert_eq!(
        store.record_policy_admission(&scope, &claim, &forged).await,
        Err(Error::OwnershipConflict)
    );
    store
        .record_policy_admission(&scope, &claim, &observation(&next, "3"))
        .await
        .unwrap();
    assert_eq!(
        store
            .activate_execution_policy(&scope, &claim, &next.digest().unwrap())
            .await,
        Err(Error::PreconditionFailed)
    );
    // Lost first patch reply: observe its desired bytes after a fresh RV CAS.
    store
        .record_policy_admission(&scope, &claim, &observation(&first, "3"))
        .await
        .unwrap();
    store
        .activate_execution_policy(&scope, &claim, &next.digest().unwrap())
        .await
        .unwrap();
    let status: (i64, String) = sqlx::query_as(
        "SELECT applied_generation,state FROM manager.execution_policy_status WHERE instance_id=$1",
    )
    .bind(client.instance_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, (1, "Applied".into()));
    let receipt:String=sqlx::query_scalar("SELECT content_digest FROM manager.execution_effective_policy_receipts WHERE instance_id=$1").bind(client.instance_id).fetch_one(&pool).await.unwrap();
    assert_eq!(receipt, policy.digest().unwrap());
    assert_eq!(
        store
            .activate_execution_policy(&scope, &claim, &next.digest().unwrap())
            .await,
        Err(Error::StaleFence)
    );
    let mutations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.execution_policy_admission_inputs WHERE instance_id=$1",
    )
    .bind(client.instance_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mutations, 2);
    assert!(
        sqlx::query("DELETE FROM manager.execution_policy_admission_inputs")
            .execute(&pool)
            .await
            .is_err()
    );
    // No worker readiness has been fabricated by policy activation.
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("after-activation"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let leases: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.execution_leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leases, 0);
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn worker_claim_reopen_and_revoked_owner_cannot_forge_activation(pool: PgPool) {
    let (store, scope, client, _, old) = activation_fixture(&pool).await;
    let plan = store.activation_plan(&scope, &old).await.unwrap();
    let original = input(&plan, &old, "1");
    store
        .prepare_policy_admission(&scope, &old, &original)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.execution_policy_status SET lease_until=clock_timestamp()-interval '1 second' WHERE instance_id=$1").bind(client.instance_id).execute(&pool).await.unwrap();
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let claim = reopened
        .claim_policy(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(claim.fence > old.fence);
    assert_eq!(
        store
            .record_policy_admission(&scope, &old, &observation(&original, "2"))
            .await,
        Err(Error::StaleFence)
    );
    let pending = reopened
        .pending_policy_admissions(&scope, &claim)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].input, original);
    reopened
        .record_policy_admission(&scope, &claim, &observation(&original, "2"))
        .await
        .unwrap();
    // A raw applied flag and even a valid digest receipt cannot replace a
    // matching active work fence and observation through the production port.
    sqlx::query("INSERT INTO manager.execution_effective_policy_receipts(instance_id,generation,epoch,content_digest)VALUES($1,1,1,$2)").bind(client.instance_id).bind(&claim.digest).execute(&pool).await.unwrap();
    assert!(
        sqlx::query(
            "UPDATE manager.execution_policy_status SET applied_generation=1 WHERE instance_id=$1"
        )
        .bind(client.instance_id)
        .execute(&pool)
        .await
        .is_err()
    );
    sqlx::query("UPDATE manager.assignments SET access='revoked',version=version+1 WHERE id=(SELECT assignment_id FROM manager.instances WHERE id=$1)").bind(client.instance_id).execute(&pool).await.unwrap();
    assert_eq!(
        reopened
            .activate_execution_policy(&scope, &claim, &original.digest().unwrap())
            .await,
        Err(Error::StaleFence)
    );
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn policy_change_commits_old_worker_drain_and_blocks_new_activation(pool: PgPool) {
    let (store, scope, client, policy, claim) = activation_fixture(&pool).await;
    let plan = store.activation_plan(&scope, &claim).await.unwrap();
    let original = input(&plan, &claim, "1");
    store
        .prepare_policy_admission(&scope, &claim, &original)
        .await
        .unwrap();
    store
        .record_policy_admission(&scope, &claim, &observation(&original, "2"))
        .await
        .unwrap();
    store
        .activate_execution_policy(&scope, &claim, &original.digest().unwrap())
        .await
        .unwrap();
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("pending-worker"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    use hermes_application::execution::{
        AllowedRuntime, AssignmentExecution, ExecutionCommand, ExecutionMutation,
    };
    sqlx::query("INSERT INTO manager.terminal_runtime_versions(id,version,schema_version,content_digest,definition) VALUES($1,1,1,$2,$3)").bind(&policy.default_runtime_id).bind(&policy.runtimes[0].definition_digest).bind(serde_json::to_value(&policy.runtimes[0].definition).unwrap()).execute(&pool).await.unwrap();
    let mutation = ExecutionMutation {
        scope: scope.clone(),
        actor: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
        },
        request_id: Uuid::new_v4(),
        idempotency_key: "change".into(),
        command: ExecutionCommand::SetPolicy {
            assignment_id: sqlx::query_scalar(
                "SELECT assignment_id FROM manager.instances WHERE id=$1",
            )
            .bind(client.instance_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            expected_version: 1,
            policy: AssignmentExecution {
                default_runtime_id: policy.default_runtime_id.clone(),
                allowed: vec![AllowedRuntime {
                    runtime: policy.runtimes[0].definition.runtime.clone(),
                    resources: policy.runtimes[0].resources.clone(),
                    mounts: vec![],
                    limits: Default::default(),
                }],
            },
        },
    };
    store.mutate_execution(mutation.clone()).await.unwrap();
    let (state,remaining):(String,f64)=sqlx::query_as("SELECT state,extract(epoch FROM drain_deadline-clock_timestamp())::float8 FROM manager.execution_environments WHERE instance_id=$1").bind(client.instance_id).fetch_one(&pool).await.unwrap();
    assert_eq!(state, "Draining");
    assert!((295.0..=300.0).contains(&remaining));
    let deadline: String = sqlx::query_scalar(
        "SELECT drain_deadline::text FROM manager.execution_environments WHERE instance_id=$1",
    )
    .bind(client.instance_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    store.mutate_execution(mutation).await.unwrap();
    let unchanged: String = sqlx::query_scalar(
        "SELECT drain_deadline::text FROM manager.execution_environments WHERE instance_id=$1",
    )
    .bind(client.instance_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(deadline, unchanged);
    let next = store
        .claim_policy(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.generation, 2);
    assert!(matches!(
        store.activation_plan(&scope, &next).await,
        Err(Error::PreconditionFailed)
    ));
    let status:(i64,i64)=sqlx::query_as("SELECT desired_generation,applied_generation FROM manager.execution_policy_status WHERE instance_id=$1").bind(client.instance_id).fetch_one(&pool).await.unwrap();
    assert_eq!(status, (2, 1));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn last_applied_number_cannot_authorize_execution_after_observation_failure(pool: PgPool) {
    let (store, scope, client, policy) = prepared(&pool, false).await;
    effective_fixture(&pool, &policy).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let lease = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("live-before-drift"))
            .await
            .unwrap(),
    );
    sqlx::query("UPDATE manager.execution_policy_status SET state='Waiting',condition='admission_pending' WHERE instance_id=$1").bind(client.instance_id).execute(&pool).await.unwrap();
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("live-before-drift"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    assert_eq!(
        store
            .heartbeat_execution_lease(&scope, &client, &LeaseRequest { lease: lease.lease })
            .await,
        Err(Error::PreconditionFailed)
    );
    let applied: i64 = sqlx::query_scalar(
        "SELECT applied_generation FROM manager.execution_policy_status WHERE instance_id=$1",
    )
    .bind(client.instance_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied, 1);
}
