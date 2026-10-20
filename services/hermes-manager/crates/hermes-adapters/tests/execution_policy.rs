//! Real PostgreSQL owner isolation, durable policy mutation, and stale fencing.
use std::collections::{BTreeMap, BTreeSet};

use hermes_adapters::postgres::{FreshInventory, PgStore};
use hermes_application::{
    AdminPolicy, Error, InstallationScope, VerifiedPrincipal,
    execution::{
        AllowedRuntime, AssignmentExecution, BackupPolicy, ExecutionAdministration,
        ExecutionCommand, ExecutionRepository, MountSelection, PermittedMount, PolicyCondition,
        PullSecretBinding, RuntimeDefinition, RuntimeRef, StorageBinding, WorkerResources,
    },
};
use hermes_domain::{Identity, instance_namespace};
use sqlx::{PgPool, Row};
use uuid::Uuid;

struct Owner {
    assignment: Uuid,
    instance: Uuid,
    namespace: String,
    namespace_uid: String,
}
struct Fixture {
    store: PgStore,
    scope: InstallationScope,
    admin: ExecutionAdministration<PgStore>,
    actor: VerifiedPrincipal,
    owners: [Owner; 2],
}
async fn fixture(pool: &PgPool) -> Fixture {
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
                cluster_identity: "execution-fixture".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO manager.instance_templates(id,version,schema_version,image,content_digest,approved) VALUES('personal',1,1,$1,$2,true)").bind(format!("ghcr.io/example/hermes@sha256:{}","a".repeat(64))).bind("b".repeat(64)).execute(pool).await.unwrap();
    let mut owners = Vec::new();
    for subject in ["owner-a", "owner-b"] {
        let identity = Uuid::new_v4();
        let assignment = Uuid::new_v4();
        let instance = Uuid::new_v4();
        let namespace = instance_namespace(scope.id, instance);
        let namespace_uid = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO manager.identities(id,issuer,subject) VALUES($1,$2,$3)")
            .bind(identity)
            .bind(&scope.issuer)
            .bind(subject)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO manager.assignments(id,installation_id,identity_id,access,template_id,template_version,version) VALUES($1,$2,$3,'enabled','personal',1,1)").bind(assignment).bind(scope.id).bind(identity).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO manager.instances(id,assignment_id,namespace,namespace_uid,desired_generation) VALUES($1,$2,$3,$4,1)").bind(instance).bind(assignment).bind(&namespace).bind(&namespace_uid).execute(pool).await.unwrap();
        owners.push(Owner {
            assignment,
            instance,
            namespace,
            namespace_uid,
        });
    }
    let actor = VerifiedPrincipal {
        identity: Identity {
            issuer: scope.issuer.clone(),
            subject: "admin".into(),
        },
        audiences: BTreeSet::from(["manager-api".into()]),
        client_roles: BTreeMap::from([(
            "manager-admin".into(),
            BTreeSet::from(["administrator".into()]),
        )]),
    };
    let admin = ExecutionAdministration::new(
        store.clone(),
        scope.clone(),
        AdminPolicy {
            issuer: scope.issuer.clone(),
            audience: "manager-api".into(),
            management_client: "manager-admin".into(),
            administrator_role: "administrator".into(),
        },
    )
    .unwrap();
    Fixture {
        store,
        scope,
        admin,
        actor,
        owners: owners.try_into().ok().unwrap(),
    }
}
fn runtime() -> RuntimeDefinition {
    RuntimeDefinition {
        schema_version: 1,
        runtime: RuntimeRef {
            id: "python-tools".into(),
            version: 1,
        },
        image: format!("ghcr.io/example/worker@sha256:{}", "c".repeat(64)),
        ssh_contract: "managed-ssh-v1".into(),
        uid: 1000,
        gid: 1000,
        service_account: "worker".into(),
        resources: WorkerResources {
            cpu_millis: 1000,
            memory_mib: 1024,
            ephemeral_storage_mib: 1024,
        },
        locale: "C.UTF-8".into(),
        egress: vec![],
        pull_secret_names: vec!["worker-pull".into()],
    }
}
fn storage(owner: &Owner) -> StorageBinding {
    StorageBinding {
        id: Uuid::new_v4(),
        version: 1,
        instance_id: owner.instance,
        namespace: owner.namespace.clone(),
        namespace_uid: owner.namespace_uid.clone(),
        pvc_name: "network-home".into(),
        pvc_uid: Uuid::new_v4().to_string(),
        backing_storage_id: Uuid::new_v4(),
        external: true,
        required: true,
        backup: BackupPolicy::External,
        mounts: vec![PermittedMount {
            path: "/mnt/network-home".into(),
            read_only: true,
            sub_path: Some("documents".into()),
        }],
    }
}
fn pull(owner: &Owner) -> PullSecretBinding {
    PullSecretBinding {
        instance_id: owner.instance,
        namespace: owner.namespace.clone(),
        namespace_uid: owner.namespace_uid.clone(),
        name: "worker-pull".into(),
        uid: Uuid::new_v4().to_string(),
    }
}
fn policy(binding: &StorageBinding) -> AssignmentExecution {
    AssignmentExecution {
        default_runtime_id: "python-tools".into(),
        allowed: vec![AllowedRuntime {
            runtime: runtime().runtime,
            resources: runtime().resources,
            mounts: vec![MountSelection {
                binding_id: binding.id,
                binding_version: binding.version,
                mount: binding.mounts[0].clone(),
            }],
        }],
    }
}
async fn register(
    f: &Fixture,
    key: &str,
    command: ExecutionCommand,
) -> Result<hermes_application::execution::ExecutionAccepted, Error> {
    f.admin
        .execute(&f.actor, Uuid::new_v4(), key, command)
        .await
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn same_catalog_image_resolves_independent_owner_claims_and_secret_uids(pool: PgPool) {
    let f = fixture(&pool).await;
    register(
        &f,
        "runtime",
        ExecutionCommand::RegisterRuntime {
            definition: runtime(),
        },
    )
    .await
    .unwrap();
    let mut policies = Vec::new();
    for (index, owner) in f.owners.iter().enumerate() {
        let binding = storage(owner);
        let secret = pull(owner);
        register(
            &f,
            &format!("storage-{index}"),
            ExecutionCommand::RegisterStorage {
                binding: binding.clone(),
            },
        )
        .await
        .unwrap();
        register(
            &f,
            &format!("pull-{index}"),
            ExecutionCommand::RegisterPullSecret {
                binding: secret.clone(),
            },
        )
        .await
        .unwrap();
        let mut selected = policy(&binding);
        if index == 1 {
            selected.allowed[0].resources.cpu_millis = 500;
        }
        let accepted = register(
            &f,
            &format!("policy-{index}"),
            ExecutionCommand::SetPolicy {
                assignment_id: owner.assignment,
                expected_version: 1,
                policy: selected.clone(),
            },
        )
        .await
        .unwrap();
        let view = f
            .store
            .execution_policy(&f.scope, owner.assignment)
            .await
            .unwrap();
        assert_eq!(
            accepted.policy_digest.as_deref(),
            Some(view.digest.as_str())
        );
        assert_eq!(accepted.assignment_version, Some(2));
        assert_eq!(view.desired_generation, 1);
        assert_eq!(view.applied_generation, 0);
        assert_eq!(view.desired.runtimes[0].definition, runtime());
        assert_eq!(
            view.desired.runtimes[0].resources,
            selected.allowed[0].resources
        );
        assert_eq!(view.desired.runtimes[0].mounts[0].binding, binding);
        assert_eq!(view.desired.runtimes[0].pull_secrets, vec![secret]);
        assert_eq!(
            f.store
                .owner_storage_bindings(&f.scope, owner.instance, None, 10)
                .await
                .unwrap(),
            vec![binding]
        );
        policies.push(view);
    }
    assert_ne!(policies[0].digest, policies[1].digest);
    assert_ne!(
        policies[0].desired.runtimes[0].mounts[0].binding.pvc_uid,
        policies[1].desired.runtimes[0].mounts[0].binding.pvc_uid
    );
    assert_ne!(
        policies[0].desired.runtimes[0].pull_secrets[0].uid,
        policies[1].desired.runtimes[0].pull_secrets[0].uid
    );
    for statement in [
        "UPDATE manager.terminal_runtime_versions SET content_digest=repeat('d',64)",
        "DELETE FROM manager.storage_binding_versions",
        "UPDATE manager.storage_binding_owners SET instance_id=gen_random_uuid()",
        "DELETE FROM manager.execution_pull_secrets",
        "UPDATE manager.execution_policy_versions SET policy='{}'",
        "DELETE FROM manager.execution_configuration_events",
        "UPDATE manager.execution_policy_status SET applied_generation=1",
        "UPDATE manager.execution_policy_status SET instance_id=gen_random_uuid()",
        "UPDATE manager.execution_policy_status SET created_at=created_at+interval '1 second'",
        "UPDATE manager.execution_policy_status SET epoch=epoch+1",
        "UPDATE manager.execution_policy_status SET fence=fence+2",
        "DELETE FROM manager.execution_policy_status",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "mutable protected record: {statement}"
        );
    }
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.execution_configuration_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 7);
    assert_eq!(
        f.store
            .runtime_versions(&f.scope, "python-tools", 0, 10)
            .await
            .unwrap(),
        vec![runtime()]
    );
    assert!(
        f.store
            .runtime_versions(&f.scope, "python-tools", 1, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let serialized: String = sqlx::query_scalar(
        "SELECT string_agg(change::text,'') FROM manager.execution_configuration_events",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!serialized.contains("network-home"));
    assert!(!serialized.contains("documents"));
    assert!(!serialized.contains("pvc_uid"));
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn foreign_owner_missing_secret_and_unapproved_mount_fail_before_mutation_commit(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let a = &f.owners[0];
    let b = &f.owners[1];
    register(
        &f,
        "runtime",
        ExecutionCommand::RegisterRuntime {
            definition: runtime(),
        },
    )
    .await
    .unwrap();
    let binding = storage(a);
    register(
        &f,
        "storage",
        ExecutionCommand::RegisterStorage {
            binding: binding.clone(),
        },
    )
    .await
    .unwrap();
    let original = policy(&binding);
    assert_eq!(
        register(
            &f,
            "missing-secret",
            ExecutionCommand::SetPolicy {
                assignment_id: a.assignment,
                expected_version: 1,
                policy: original.clone()
            }
        )
        .await,
        Err(Error::OwnershipConflict)
    );
    register(
        &f,
        "pull",
        ExecutionCommand::RegisterPullSecret { binding: pull(a) },
    )
    .await
    .unwrap();
    assert_eq!(
        register(
            &f,
            "foreign-storage",
            ExecutionCommand::SetPolicy {
                assignment_id: b.assignment,
                expected_version: 1,
                policy: original.clone()
            }
        )
        .await,
        Err(Error::OwnershipConflict)
    );
    let mut changed = original.clone();
    changed.allowed[0].resources.cpu_millis = runtime().resources.cpu_millis + 1;
    assert_eq!(
        register(
            &f,
            "resource-upgrade",
            ExecutionCommand::SetPolicy {
                assignment_id: a.assignment,
                expected_version: 1,
                policy: changed
            }
        )
        .await,
        Err(Error::Invalid)
    );
    let mut changed = original.clone();
    changed.allowed[0].mounts[0].mount.read_only = false;
    assert_eq!(
        register(
            &f,
            "write-upgrade",
            ExecutionCommand::SetPolicy {
                assignment_id: a.assignment,
                expected_version: 1,
                policy: changed
            }
        )
        .await,
        Err(Error::OwnershipConflict)
    );
    let mut changed = original.clone();
    changed.allowed[0].runtime.version = 2;
    assert_eq!(
        register(
            &f,
            "missing-version",
            ExecutionCommand::SetPolicy {
                assignment_id: a.assignment,
                expected_version: 1,
                policy: changed
            }
        )
        .await,
        Err(Error::TemplateUnavailable)
    );
    let mut changed = binding.clone();
    changed.version = 2;
    changed.instance_id = b.instance;
    changed.namespace = b.namespace.clone();
    changed.namespace_uid = b.namespace_uid.clone();
    assert_eq!(
        register(
            &f,
            "owner-reassign",
            ExecutionCommand::RegisterStorage { binding: changed }
        )
        .await,
        Err(Error::OwnershipConflict)
    );
    let mut changed = binding.clone();
    changed.version = 2;
    changed.namespace_uid = b.namespace_uid.clone();
    assert_eq!(
        register(
            &f,
            "namespace-rebind",
            ExecutionCommand::RegisterStorage { binding: changed }
        )
        .await,
        Err(Error::OwnershipConflict)
    );
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM manager.mutation_requests WHERE idempotency_key IN ('missing-secret','foreign-storage','write-upgrade','missing-version','owner-reassign','namespace-rebind')").fetch_one(&pool).await.unwrap();
    assert_eq!(failures, 0);
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.execution_configuration_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 3);
    let mut unauthorized = f.actor.clone();
    unauthorized.client_roles.clear();
    assert_eq!(
        f.admin
            .execute(
                &unauthorized,
                Uuid::new_v4(),
                "unauthorized",
                ExecutionCommand::SetPolicy {
                    assignment_id: a.assignment,
                    expected_version: 1,
                    policy: original
                }
            )
            .await,
        Err(Error::Forbidden)
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_idempotency_versions_reopen_and_fenced_policy_waits(pool: PgPool) {
    let f = fixture(&pool).await;
    let a = &f.owners[0];
    let command = ExecutionCommand::RegisterRuntime {
        definition: runtime(),
    };
    let (first, second) = tokio::join!(
        register(&f, "catalog-once", command.clone()),
        register(&f, "catalog-once", command.clone())
    );
    assert_eq!(first.unwrap(), second.unwrap());
    let mut changed = runtime();
    changed.uid = 1001;
    assert_eq!(
        register(
            &f,
            "catalog-once",
            ExecutionCommand::RegisterRuntime {
                definition: changed
            }
        )
        .await,
        Err(Error::IdempotencyConflict)
    );
    let binding = storage(a);
    register(
        &f,
        "storage",
        ExecutionCommand::RegisterStorage {
            binding: binding.clone(),
        },
    )
    .await
    .unwrap();
    register(
        &f,
        "pull",
        ExecutionCommand::RegisterPullSecret { binding: pull(a) },
    )
    .await
    .unwrap();
    let command = ExecutionCommand::SetPolicy {
        assignment_id: a.assignment,
        expected_version: 1,
        policy: policy(&binding),
    };
    let (left, right) = tokio::join!(
        register(&f, "first-policy", command.clone()),
        register(&f, "competing-policy", command)
    );
    assert!(matches!(
        (&left, &right),
        (Ok(_), Err(Error::PreconditionFailed)) | (Err(Error::PreconditionFailed), Ok(_))
    ));
    let worker = Uuid::new_v4();
    let claim = f
        .store
        .claim_policy(&f.scope, worker, 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.store
            .policy_plan(&f.scope, &claim)
            .await
            .unwrap()
            .digest()
            .unwrap(),
        claim.digest
    );
    let reopened = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    assert!(
        reopened
            .claim_policy(&f.scope, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let mut stale = claim.clone();
    stale.fence += 1;
    assert_eq!(
        reopened
            .defer_policy(&f.scope, &stale, PolicyCondition::DrainPending, 5)
            .await,
        Err(Error::StaleFence)
    );
    sqlx::query("UPDATE manager.execution_policy_status SET lease_until=clock_timestamp()-interval '1 second'").execute(&pool).await.unwrap();
    let newer = reopened
        .claim_policy(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(newer.fence > claim.fence);
    assert_eq!(
        f.store
            .defer_policy(&f.scope, &claim, PolicyCondition::DrainPending, 5)
            .await,
        Err(Error::StaleFence)
    );
    register(
        &f,
        "new-policy",
        ExecutionCommand::SetPolicy {
            assignment_id: a.assignment,
            expected_version: 2,
            policy: policy(&binding),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.policy_plan(&f.scope, &newer).await,
        Err(Error::StaleFence)
    );
    let new_claim = reopened
        .claim_policy(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_claim.generation, 2);
    reopened
        .defer_policy(&f.scope, &new_claim, PolicyCondition::DrainPending, 5)
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT state,condition,applied_generation FROM manager.execution_policy_status",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state"), "Waiting");
    assert_eq!(row.get::<String, _>("condition"), "drain_pending");
    assert_eq!(row.get::<i64, _>("applied_generation"), 0);
    sqlx::query("UPDATE manager.execution_policy_status SET next_attempt_at=clock_timestamp()-interval '1 second'").execute(&pool).await.unwrap();
    let disabled_claim = reopened
        .claim_policy(&f.scope, Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE manager.assignments SET access='suspended' WHERE id=$1")
        .bind(a.assignment)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        reopened.policy_plan(&f.scope, &disabled_claim).await,
        Err(Error::StaleFence)
    );
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .claim_policy(&f.scope, Uuid::new_v4(), 30)
            .await
            .unwrap_err(),
        Error::Maintenance
    );
}

#[test]
fn typed_policy_rejects_raw_specs_mount_traversal_overlap_and_root_ssh_workers() {
    let mut definition = runtime();
    definition.uid = 0;
    assert_eq!(definition.validate(), Err(Error::Invalid));
    let mut definition = runtime();
    definition.image = "registry.example/worker:latest".into();
    assert_eq!(definition.validate(), Err(Error::Invalid));
    let mut definition = runtime();
    definition.service_account = "administrator".into();
    assert_eq!(definition.validate(), Err(Error::Invalid));
    let mut value = serde_json::to_value(runtime()).unwrap();
    value["env"] = serde_json::json!({"TOKEN":"secret"});
    assert!(serde_json::from_value::<RuntimeDefinition>(value).is_err());
    for path in [
        "/home/worker",
        "/etc/ssh/authorized_keys",
        "/mnt/../run",
        "/mnt/home/",
        "/mnt//home",
        "/workspace/./home",
    ] {
        assert_eq!(
            PermittedMount {
                path: path.into(),
                read_only: true,
                sub_path: None
            }
            .validate(),
            Err(Error::Invalid)
        );
    }
    assert_eq!(
        PermittedMount {
            path: "/mnt/home".into(),
            read_only: true,
            sub_path: Some("../other".into())
        }
        .validate(),
        Err(Error::Invalid)
    );
    let binding = StorageBinding {
        id: Uuid::new_v4(),
        version: 1,
        instance_id: Uuid::new_v4(),
        namespace: "own".into(),
        namespace_uid: "uid".into(),
        pvc_name: "home".into(),
        pvc_uid: "pvc".into(),
        backing_storage_id: Uuid::new_v4(),
        external: false,
        required: true,
        backup: BackupPolicy::PlatformCheckpoint,
        mounts: vec![PermittedMount {
            path: "/mnt/home".into(),
            read_only: true,
            sub_path: None,
        }],
    };
    let mut selected = policy(&binding);
    let mut duplicate = selected.allowed[0].mounts[0].clone();
    duplicate.mount.path = "/mnt/home/nested".into();
    selected.allowed[0].mounts.push(duplicate);
    assert_eq!(selected.validate(), Err(Error::Invalid));
}
