use hermes_adapters::{
    materials::MaterialVault,
    postgres::{FreshInventory, PgStore},
    session_crypto::Sealer,
};
use hermes_application::{
    AdminPolicy, AssignmentCommand, Assignments, Error, InstallationScope, OperationClaim,
    OperationQueue, VerifiedPrincipal,
    terminal::clients::{ExecutionClientRepository, ExecutionTokenHash, NATIVE_EXECUTION_PROFILE},
};
use hermes_domain::{Access, Identity, TemplateVersion};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn cipher() -> Sealer {
    Sealer::new(
        "controller-key".into(),
        BTreeMap::from([("controller-key".into(), vec![42; 32])]),
    )
    .unwrap()
}
fn actor(scope: &InstallationScope) -> VerifiedPrincipal {
    VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["administrator".into()]),
        )]),
    }
}
fn policy(scope: &InstallationScope) -> AdminPolicy {
    AdminPolicy {
        issuer: scope.issuer.clone(),
        audience: "manager-api".into(),
        management_client: "manager-admin".into(),
        administrator_role: "administrator".into(),
    }
}
async fn fixture(
    pool: &PgPool,
) -> (
    PgStore,
    InstallationScope,
    OperationClaim,
    Assignments<PgStore>,
) {
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    store
        .initialize_fresh(
            &scope,
            FreshInventory {
                cluster_identity: "material-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal-default',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("a".repeat(64)).execute(pool).await.unwrap();
    let service = Assignments::new(store.clone(), scope.clone(), policy(&scope)).unwrap();
    service
        .execute(
            &actor(&scope),
            Uuid::new_v4(),
            "create",
            AssignmentCommand::Create {
                identity: Identity {
                    issuer: scope.issuer.clone(),
                    subject: "owner-a".into(),
                },
                access: Access::Enabled,
                template: TemplateVersion {
                    id: "personal-default".into(),
                    version: 1,
                },
            },
        )
        .await
        .unwrap();
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    (store, scope, claim, service)
}

use hermes_application::{
    execution::{ResolvedPolicy, ResolvedRuntime, RuntimeDefinition, RuntimeRef, WorkerResources},
    terminal::{clients::ExecutionClient, leases::*},
};
async fn prepared(
    pool: &PgPool,
    exclusive: bool,
) -> (PgStore, InstallationScope, ExecutionClient, ResolvedPolicy) {
    let (store, scope, claim, _) = fixture(pool).await;
    let vault = MaterialVault::new(store.clone(), cipher());
    let (material, bootstrap) = vault
        .native_bootstrap(
            &scope,
            &claim,
            TemplateVersion {
                id: "personal-default".into(),
                version: 1,
            },
            "https://manager.example.org/execution",
        )
        .await
        .unwrap();
    let client = store
        .register_under_current_claim(
            &scope,
            &claim,
            &material.context,
            ExecutionTokenHash::from_token(&bootstrap.execution_client_token).unwrap(),
            NATIVE_EXECUTION_PROFILE,
        )
        .await
        .unwrap();
    let namespace: String =
        sqlx::query_scalar("SELECT namespace FROM manager.instances WHERE id=$1")
            .bind(claim.instance_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let ns_uid = Uuid::new_v4().to_string();
    sqlx::query("UPDATE manager.instances SET namespace_uid=$2 WHERE id=$1")
        .bind(claim.instance_id)
        .bind(&ns_uid)
        .execute(pool)
        .await
        .unwrap();
    // Trusted observer fixture: use the real durable Namespace reservation and
    // verified fence ports; no Kubernetes/SSH success is inferred from SQL state.
    use hermes_application::{
        namespace_fence::{Admission, NamespaceFenceObservation, NamespaceFenceRepository},
        resources::{ResourceObservation, ResourceRepository, ResourceVersion},
    };
    use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention};
    let spec = ResourceSpec {
        kind: ResourceKind::Namespace,
        namespace: None,
        namespace_uid: None,
        name: namespace.clone(),
        manifest_digest: "b".repeat(64),
        purpose: "namespace".into(),
        retention: Retention::Retain,
        encrypted_material_ref: None,
    };
    store
        .prepare_resource(&scope, &claim, "namespace", spec.clone())
        .await
        .unwrap();
    store
        .bind_resource(
            &scope,
            &claim,
            "namespace",
            ResourceObservation {
                uid: ns_uid.clone(),
                resource_version: "1".into(),
                manifest_digest: spec.manifest_digest,
            },
        )
        .await
        .unwrap();
    let open = store
        .prepare_namespace_fence(
            &scope,
            &claim,
            "open",
            Admission::Open,
            ResourceVersion {
                uid: ns_uid.clone(),
                resource_version: "1".into(),
            },
        )
        .await
        .unwrap();
    store
        .complete_namespace_fence(
            &scope,
            &claim,
            "open",
            NamespaceFenceObservation {
                uid: ns_uid.clone(),
                resource_version: "2".into(),
                input_digest: open.input.digest().unwrap(),
                state: open.input.state,
            },
        )
        .await
        .unwrap();
    let definition = RuntimeDefinition {
        schema_version: 1,
        runtime: RuntimeRef {
            id: "python-tools".into(),
            version: 1,
        },
        image: format!("ghcr.io/example/worker@sha256:{}", "a".repeat(64)),
        ssh_contract: "managed-ssh-v1".into(),
        uid: 1000,
        gid: 1000,
        service_account: "worker".into(),
        resources: WorkerResources {
            cpu_millis: 100,
            memory_mib: 128,
            ephemeral_storage_mib: 128,
        },
        locale: "C.UTF-8".into(),
        egress: vec![],
        pull_secret_names: vec![],
    };
    let p = ResolvedPolicy {
        installation_id: scope.id,
        instance_id: claim.instance_id,
        namespace,
        namespace_uid: ns_uid,
        generation: 1,
        default_runtime_id: "python-tools".into(),
        runtimes: vec![ResolvedRuntime {
            definition_digest: definition.digest().unwrap(),
            resources: definition.resources.clone(),
            definition,
            mounts: vec![],
            pull_secrets: vec![],
            limits: ExecutionLimits {
                exclusive,
                ..Default::default()
            },
        }],
    };
    sqlx::query("INSERT INTO manager.execution_policy_versions(instance_id,generation,content_digest,policy)VALUES($1,1,$2,$3)").bind(claim.instance_id).bind(p.digest().unwrap()).bind(serde_json::to_value(&p).unwrap()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO manager.execution_policy_status(instance_id,desired_generation,state,epoch)VALUES($1,1,'Pending',1)").bind(claim.instance_id).execute(pool).await.unwrap();
    (store, scope, client, p)
}
// Explicit trusted lease test fixture ONLY. Activation adapter tests separately
// exercise the production readback guard; lease tests seed that trusted boundary.
async fn effective_fixture(pool: &PgPool, p: &ResolvedPolicy) {
    sqlx::query(
        "ALTER TABLE manager.execution_policy_status DISABLE TRIGGER verified_applied_policy",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO manager.execution_effective_policy_receipts(instance_id,generation,epoch,content_digest)VALUES($1,1,1,$2)").bind(p.instance_id).bind(p.digest().unwrap()).execute(pool).await.unwrap();
    sqlx::query(
        "UPDATE manager.execution_policy_status SET applied_generation=1,state='Applied' WHERE instance_id=$1",
    )
    .bind(p.instance_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "ALTER TABLE manager.execution_policy_status ENABLE TRIGGER verified_applied_policy",
    )
    .execute(pool)
    .await
    .unwrap();
}
fn request(key: &str) -> AcquireRequest {
    AcquireRequest {
        profile: "default".into(),
        session: "session-a".into(),
        mode: LifetimeMode::PerSession,
        request: key.into(),
        runtime_id: None,
        parent_lease: None,
        runtime: Some(InvocationContext {
            hermes_home: "/mnt/hermes".into(),
            workdir: "/workspace".into(),
        }),
    }
}
async fn ready_fixture(
    pool: &PgPool,
    store: &PgStore,
    scope: &InstallationScope,
    client: &ExecutionClient,
) -> EnvironmentClaim {
    assert_eq!(
        store
            .acquire_execution_lease(scope, client, &request("acquire-a"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let claim = store
        .claim_execution_environment(scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let intent = store
        .prepare_execution_worker(scope, &claim, &"c".repeat(64))
        .await
        .unwrap();
    let binding = WorkerBinding {
        gated: true,
        pod_uid: Uuid::new_v4().to_string(),
        resource_version: "7".into(),
        observed_spec_digest: "d".repeat(64),
    };
    let bound = store
        .bind_execution_worker(scope, &intent, &binding)
        .await
        .unwrap();
    assert_eq!(bound.resource_version, "7");
    let recovered = store
        .bind_execution_worker(
            scope,
            &intent,
            &WorkerBinding {
                gated: false,
                resource_version: "9".into(),
                ..binding.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered.resource_version, "9");
    let original_rv: String = sqlx::query_scalar("SELECT resource_version FROM manager.execution_worker_start_intents WHERE environment_id=$1")
        .bind(claim.environment_id).fetch_one(pool).await.unwrap();
    assert_eq!(original_rv, "7");
    let input_versions: Vec<String> = sqlx::query_scalar("SELECT resource_version FROM manager.execution_worker_start_inputs WHERE environment_id=$1 ORDER BY resource_version")
        .bind(claim.environment_id).fetch_all(pool).await.unwrap();
    assert_eq!(input_versions, vec!["7", "9"]);
    store
        .record_execution_worker_ready(
            scope,
            &intent,
            &WorkerReady {
                host_key: None,
                pod_uid: binding.pod_uid,
                resource_version: "10".into(),
                spec_digest: intent.spec_digest.clone(),
                observed_spec_digest: binding.observed_spec_digest,
                host: "10.0.0.8".into(),
            },
        )
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.execution_worker_start_intents")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    claim
}
fn granted(value: AcquireOutcome) -> AcquireReply {
    match value {
        AcquireOutcome::Granted(reply) => reply,
        other => panic!("expected grant: {other:?}"),
    }
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn native_readiness_does_not_drain_or_wait_for_terminal_workers(pool: PgPool) {
    let (store, scope, client, policy) = prepared(&pool, false).await;
    effective_fixture(&pool, &policy).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    // Acquisition updates last_used_at while the read-only observer's fenced
    // claim remains held. It must neither fail nor steal that observer claim.
    let observation = store
        .claim_ready_worker_observation(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    let unfenced = sqlx::query("UPDATE manager.execution_environments SET lease_until=lease_until+interval '1 second' WHERE id=$1")
        .bind(observation.claim.environment_id).execute(&pool).await.unwrap_err();
    assert_eq!(
        unfenced
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("23514")
    );
    let lease = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    let mut pending = request("another-session");
    pending.session = "session-b".into();
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &pending)
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    // Reclaim the native operation through the normal fenced repository port.
    sqlx::query("UPDATE manager.operations SET lease_until=clock_timestamp()-interval '1 second' WHERE state='Running'")
        .execute(&pool).await.unwrap();
    let claim = store
        .claim(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.complete_native_commissioning(&scope, &claim).await,
        Err(Error::Forbidden)
    ));
    sqlx::query("UPDATE manager.assignments SET infrastructure_managed=true")
        .execute(&pool)
        .await
        .unwrap();
    let mut stale = claim.clone();
    stale.fence -= 1;
    assert!(matches!(
        store.complete_native_commissioning(&scope, &stale).await,
        Err(Error::StaleFence)
    ));
    store
        .complete_native_commissioning(&scope, &claim)
        .await
        .unwrap();
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM manager.execution_environments ORDER BY state")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(states, vec!["Pending", "Ready"]);
    let state: String = sqlx::query_scalar("SELECT state FROM manager.instances")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "Ready");
    store
        .heartbeat_execution_lease(&scope, &client, &LeaseRequest { lease: lease.lease })
        .await
        .unwrap();
    store
        .mark_execution_worker_lost(&scope, &observation)
        .await
        .unwrap();
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn no_grant_without_effective_policy_and_recorded_bound_ready(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    assert!(
        store
            .claim_execution_environment(&scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM manager.execution_leases")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    effective_fixture(&pool, &p).await;
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let claim = store
        .claim_execution_environment(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let intent = store
        .prepare_execution_worker(&scope, &claim, &"c".repeat(64))
        .await
        .unwrap();
    assert!(matches!(
        store
            .bind_execution_worker(
                &scope,
                &intent,
                &WorkerBinding {
                    gated: false,
                    pod_uid: Uuid::new_v4().to_string(),
                    resource_version: "1".into(),
                    observed_spec_digest: "d".repeat(64)
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert!(matches!(
        store
            .record_execution_worker_ready(
                &scope,
                &intent,
                &WorkerReady {
                    host_key: None,
                    pod_uid: Uuid::new_v4().to_string(),
                    resource_version: "1".into(),
                    spec_digest: intent.spec_digest.clone(),
                    observed_spec_digest: "d".repeat(64),
                    host: "10.0.0.8".into()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let mut changed = request("acquire-a");
    changed.session = "changed".into();
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &changed)
            .await,
        Err(Error::IdempotencyConflict)
    ));
    assert!(
        sqlx::query("UPDATE manager.execution_worker_intents SET spec_digest=$1")
            .bind("e".repeat(64))
            .execute(&pool)
            .await
            .is_err()
    );
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn replay_never_renews_and_released_expired_leases_are_terminal(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    effective_fixture(&pool, &p).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let first = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    let original: String =
        sqlx::query_scalar("SELECT expires_at::text FROM manager.execution_leases WHERE id=$1")
            .bind(first.lease)
            .fetch_one(&pool)
            .await
            .unwrap();
    let replay = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    assert_eq!(first.lease, replay.lease);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT expires_at::text FROM manager.execution_leases WHERE id=$1"
        )
        .bind(first.lease)
        .fetch_one(&pool)
        .await
        .unwrap(),
        original
    );
    let lease = LeaseRequest { lease: first.lease };
    store
        .heartbeat_execution_lease(&scope, &client, &lease)
        .await
        .unwrap();
    store
        .release_execution_lease(&scope, &client, &lease)
        .await
        .unwrap();
    store
        .release_execution_lease(&scope, &client, &lease)
        .await
        .unwrap();
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await,
        Err(Error::PreconditionFailed)
    ));
    assert!(matches!(
        store
            .heartbeat_execution_lease(&scope, &client, &lease)
            .await,
        Err(Error::PreconditionFailed)
    ));
    // Trusted test clock fixture: initially expired rows, not a production write.
    let second = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-b"))
            .await
            .unwrap(),
    );
    sqlx::query("ALTER TABLE manager.execution_leases DISABLE TRIGGER immutable_execution_lease")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.execution_leases SET granted_at=clock_timestamp()-interval '3 minutes',original_expires_at=clock_timestamp()-interval '1 second',expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(second.lease).execute(&pool).await.unwrap();
    sqlx::query("ALTER TABLE manager.execution_leases ENABLE TRIGGER immutable_execution_lease")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(store.expire_execution_leases(&scope, 1).await.unwrap(), 1);
    assert_eq!(store.expire_execution_leases(&scope, 1).await.unwrap(), 0);
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-b"))
            .await,
        Err(Error::PreconditionFailed)
    ));
    assert!(sqlx::query("UPDATE manager.execution_leases SET state='Active',terminal_at=NULL,fence=fence+1 WHERE id=$1").bind(second.lease).execute(&pool).await.is_err());
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_exclusive_grant_and_scoped_parent_delegation(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, true).await;
    effective_fixture(&pool, &p).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let a = request("race-a");
    let b = request("race-b");
    let (aa, bb) = tokio::join!(
        store.acquire_execution_lease(&scope, &client, &a),
        store.acquire_execution_lease(&scope, &client, &b)
    );
    let results = [aa.unwrap(), bb.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, AcquireOutcome::Granted(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, AcquireOutcome::Pending))
            .count(),
        1
    );
    let parent = results
        .into_iter()
        .find_map(|r| {
            if let AcquireOutcome::Granted(g) = r {
                Some(g)
            } else {
                None
            }
        })
        .unwrap();
    let mut child = request("delegate");
    child.parent_lease = Some(parent.lease);
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &child)
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let mut foreign = client.clone();
    foreign.instance_id = Uuid::new_v4();
    assert!(matches!(
        store
            .release_execution_lease(
                &scope,
                &foreign,
                &LeaseRequest {
                    lease: parent.lease
                }
            )
            .await,
        Err(Error::Forbidden)
    ));
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn live_parent_join_release_independence_close_scope_and_epoch_fence(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    effective_fixture(&pool, &p).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let parent = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    let mut child = request("delegate");
    child.parent_lease = Some(parent.lease);
    let child_grant = granted(
        store
            .acquire_execution_lease(&scope, &client, &child)
            .await
            .unwrap(),
    );
    store
        .release_execution_lease(
            &scope,
            &client,
            &LeaseRequest {
                lease: parent.lease,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .heartbeat_execution_lease(
                &scope,
                &client,
                &LeaseRequest {
                    lease: child_grant.lease
                }
            )
            .await
            .is_ok()
    );
    let mut late = child.clone();
    late.request = "late-parent-join".into();
    assert!(matches!(
        store.acquire_execution_lease(&scope, &client, &late).await,
        Err(Error::Forbidden)
    ));
    let wrong = CloseSessionRequest {
        profile: "default".into(),
        session: "session-a".into(),
        runtime_id: Some("foreign".into()),
        runtime: request("x").runtime,
    };
    assert!(matches!(
        store.close_execution_session(&scope, &client, &wrong).await,
        Err(Error::Forbidden)
    ));
    let good = CloseSessionRequest {
        runtime_id: None,
        ..wrong
    };
    store
        .close_execution_session(&scope, &client, &good)
        .await
        .unwrap();
    assert!(matches!(
        store
            .heartbeat_execution_lease(
                &scope,
                &client,
                &LeaseRequest {
                    lease: child_grant.lease
                }
            )
            .await,
        Err(Error::PreconditionFailed)
    ));
    let listed = store
        .list_execution_environments(&scope, &client)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].stale);
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.list_execution_environments(&scope, &client).await,
        Err(Error::Maintenance)
    ));
    sqlx::query("UPDATE manager.installations SET mode='active',epoch=epoch+1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.list_execution_environments(&scope, &client).await,
        Err(Error::StaleFence) | Err(Error::RecoveryRequired)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn expired_parent_preserves_independent_live_child_and_latest_closed_fence_denies(
    pool: PgPool,
) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    effective_fixture(&pool, &p).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let parent = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    let mut req = request("child");
    req.parent_lease = Some(parent.lease);
    let child = granted(
        store
            .acquire_execution_lease(&scope, &client, &req)
            .await
            .unwrap(),
    );
    // Move only the parent's clock into the past using an explicit trusted fixture.
    sqlx::query("ALTER TABLE manager.execution_leases DISABLE TRIGGER immutable_execution_lease")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE manager.execution_leases SET granted_at=clock_timestamp()-interval '3 minutes',original_expires_at=clock_timestamp()-interval '1 second',expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(parent.lease).execute(&pool).await.unwrap();
    sqlx::query("ALTER TABLE manager.execution_leases ENABLE TRIGGER immutable_execution_lease")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(store.expire_execution_leases(&scope, 1).await.unwrap(), 1);
    store
        .heartbeat_execution_lease(&scope, &client, &LeaseRequest { lease: child.lease })
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM manager.execution_environments WHERE id=$1"
        )
        .bind(child.environment)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "Ready"
    );
    req.request = "late".into();
    assert!(matches!(
        store.acquire_execution_lease(&scope, &client, &req).await,
        Err(Error::Forbidden)
    ));
    // A later committed Closed observation supersedes historical Open even while
    // assignment access/generation and the effective-policy receipt still match.
    use hermes_application::{
        namespace_fence::{Admission, NamespaceFenceObservation, NamespaceFenceRepository},
        resources::ResourceVersion,
    };
    let row:(Uuid,Uuid,i64,i64,i64)=sqlx::query_as("SELECT id,lease_owner,generation,epoch,fence FROM manager.operations WHERE instance_id=$1 AND state='Running'").bind(client.instance_id).fetch_one(&pool).await.unwrap();
    let claim = OperationClaim {
        id: row.0,
        worker: row.1,
        instance_id: client.instance_id,
        generation: row.2,
        epoch: row.3,
        fence: row.4,
        kind: "commission".into(),
    };
    let close = store
        .prepare_namespace_fence(
            &scope,
            &claim,
            "later-close",
            Admission::Closed,
            ResourceVersion {
                uid: p.namespace_uid.clone(),
                resource_version: "2".into(),
            },
        )
        .await
        .unwrap();
    store
        .complete_namespace_fence(
            &scope,
            &claim,
            "later-close",
            NamespaceFenceObservation {
                uid: p.namespace_uid,
                resource_version: "3".into(),
                input_digest: close.input.digest().unwrap(),
                state: close.input.state,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("after-close"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    assert!(matches!(
        store
            .heartbeat_execution_lease(&scope, &client, &LeaseRequest { lease: child.lease })
            .await,
        Err(Error::PreconditionFailed)
    ));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn restarted_worker_preserves_original_intent_and_fences_stale_completion(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    effective_fixture(&pool, &p).await;
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let old = store
        .claim_execution_environment(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    let original = store
        .prepare_execution_worker(&scope, &old, &"c".repeat(64))
        .await
        .unwrap();
    // Trusted DB clock fixture simulates crash and claim expiry without sleeping.
    sqlx::query("UPDATE manager.execution_environments SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(old.environment_id).execute(&pool).await.unwrap();
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let current = reopened
        .claim_execution_environment(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.environment_id, old.environment_id);
    assert_eq!(current.generation, old.generation);
    assert!(current.fence > old.fence);
    assert!(matches!(
        store
            .prepare_execution_worker(&scope, &old, &"c".repeat(64))
            .await,
        Err(Error::StaleFence)
    ));
    let recovered = reopened
        .prepare_execution_worker(&scope, &current, &"c".repeat(64))
        .await
        .unwrap();
    assert_eq!(recovered.spec_digest, original.spec_digest);
    assert!(matches!(
        store
            .bind_execution_worker(
                &scope,
                &original,
                &WorkerBinding {
                    gated: true,
                    pod_uid: Uuid::new_v4().to_string(),
                    resource_version: "stale".into(),
                    observed_spec_digest: "d".repeat(64),
                }
            )
            .await,
        Err(Error::StaleFence)
    ));
    let stale_inputs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.execution_worker_start_inputs WHERE resource_version='stale'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale_inputs, 0);

    assert!(matches!(
        reopened
            .prepare_execution_worker(&scope, &current, &"e".repeat(64))
            .await,
        Err(Error::OwnershipConflict)
    ));
    let binding = WorkerBinding {
        gated: true,
        pod_uid: Uuid::new_v4().to_string(),
        resource_version: "7".into(),
        observed_spec_digest: "d".repeat(64),
    };
    reopened
        .bind_execution_worker(&scope, &recovered, &binding)
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .bind_execution_worker(
                &scope,
                &recovered,
                &WorkerBinding {
                    pod_uid: Uuid::new_v4().to_string(),
                    ..binding.clone()
                }
            )
            .await,
        Err(Error::OwnershipConflict)
    ));
    let ready = WorkerReady {
        host_key: None,
        pod_uid: binding.pod_uid,
        resource_version: "9".into(),
        spec_digest: recovered.spec_digest.clone(),
        observed_spec_digest: binding.observed_spec_digest,
        host: "10.0.0.8".into(),
    };
    assert!(matches!(
        store
            .record_execution_worker_ready(&scope, &original, &ready)
            .await,
        Err(Error::StaleFence)
    ));
    reopened
        .record_execution_worker_ready(&scope, &recovered, &ready)
        .await
        .unwrap();
    let lease = granted(
        reopened
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    assert_eq!(lease.environment, old.environment_id);
    assert_eq!(lease.generation, "1");
    assert!(
        sqlx::query("UPDATE manager.execution_environments SET state='Provisioning' WHERE id=$1")
            .bind(old.environment_id)
            .execute(&pool)
            .await
            .is_err()
    );
}
#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn policy_quotas_bound_durable_pending_and_active_leases(pool: PgPool) {
    let (store, scope, client, p) = prepared(&pool, false).await;
    effective_fixture(&pool, &p).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    for index in 0..32 {
        let lease = granted(
            store
                .acquire_execution_lease(&scope, &client, &request(&format!("quota-{index}")))
                .await
                .unwrap(),
        );
        assert_eq!(lease.generation, "1");
    }
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &request("quota-overflow"))
            .await,
        Err(Error::Unavailable)
    ));
    for index in 1..8 {
        let mut r = request(&format!("environment-{index}"));
        r.session = format!("session-{index}");
        assert_eq!(
            store
                .acquire_execution_lease(&scope, &client, &r)
                .await
                .unwrap(),
            AcquireOutcome::Pending
        );
    }
    let mut r = request("environment-overflow");
    r.session = "session-overflow".into();
    assert!(matches!(
        store.acquire_execution_lease(&scope, &client, &r).await,
        Err(Error::Unavailable)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM manager.execution_leases")
            .fetch_one(&pool)
            .await
            .unwrap(),
        32
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM manager.execution_environments")
            .fetch_one(&pool)
            .await
            .unwrap(),
        8
    );
}

#[path = "execution_leases/activation.rs"]
mod activation;

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn terminal_retirement_waits_for_children_fences_claims_and_preserves_request_tombstones(
    pool: PgPool,
) {
    let (store, scope, client, policy) = prepared(&pool, false).await;
    effective_fixture(&pool, &policy).await;
    let original = ready_fixture(&pool, &store, &scope, &client).await;
    let lease = granted(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await
            .unwrap(),
    );
    store
        .close_execution_session(
            &scope,
            &client,
            &CloseSessionRequest {
                profile: "default".into(),
                session: "session-a".into(),
                runtime_id: None,
                runtime: request("unused").runtime,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .claim_worker_retirement(&scope, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    store
        .release_execution_lease(&scope, &client, &LeaseRequest { lease: lease.lease })
        .await
        .unwrap();
    let old = store
        .claim_worker_retirement(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.claim.environment_id, original.environment_id);
    assert!(matches!(
        store
            .complete_worker_retirement(&scope, &old, Some("foreign-uid"))
            .await,
        Err(Error::OwnershipConflict)
    ));
    sqlx::query("UPDATE manager.execution_environments SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(original.environment_id).execute(&pool).await.unwrap();
    let current = store
        .claim_worker_retirement(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .complete_worker_retirement(&scope, &old, old.pod_uid.as_deref())
            .await,
        Err(Error::StaleFence)
    ));
    // This SQL fixture invokes the trusted observed-absence commit. The real
    // Kubernetes fixture separately verifies the exact UID disappeared.
    let mut missing_intent = current.clone();
    missing_intent.intent = None;
    missing_intent.pod_uid = None;
    assert!(matches!(
        store
            .complete_worker_retirement(&scope, &missing_intent, None)
            .await,
        Err(Error::RecoveryRequired)
    ));
    store
        .complete_worker_retirement(&scope, &current, current.pod_uid.as_deref())
        .await
        .unwrap();
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await,
        Err(Error::PreconditionFailed)
    ));
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("new-request"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
    let next = store
        .claim_execution_environment(&scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(next.environment_id, original.environment_id);
    let absent: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.execution_worker_absence")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(absent, 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn ready_worker_observation_is_fenced_and_lost_endpoint_cannot_grant_again(pool: PgPool) {
    let (store, scope, client, policy) = prepared(&pool, false).await;
    effective_fixture(&pool, &policy).await;
    ready_fixture(&pool, &store, &scope, &client).await;
    let original = store
        .claim_ready_worker_observation(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .claim_ready_worker_observation(&scope, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE manager.execution_environments SET lease_until=clock_timestamp()-interval '1 second',fence=fence+1 WHERE id=$1")
        .bind(original.claim.environment_id).execute(&pool).await.unwrap();
    let current = store
        .claim_ready_worker_observation(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.mark_execution_worker_lost(&scope, &original).await,
        Err(Error::StaleFence)
    ));
    store
        .mark_execution_worker_lost(&scope, &current)
        .await
        .unwrap();
    assert!(matches!(
        store
            .acquire_execution_lease(&scope, &client, &request("acquire-a"))
            .await,
        Err(Error::PreconditionFailed)
    ));
    let retirement = store
        .claim_worker_retirement(&scope, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retirement.claim.environment_id,
        current.claim.environment_id
    );
    store
        .complete_worker_retirement(&scope, &retirement, retirement.pod_uid.as_deref())
        .await
        .unwrap();
    assert_eq!(
        store
            .acquire_execution_lease(&scope, &client, &request("replacement-request"))
            .await
            .unwrap(),
        AcquireOutcome::Pending
    );
}
