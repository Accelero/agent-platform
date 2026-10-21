//! Actual plugin HTTPS -> controller -> non-root SSH with owner PVC subpaths.
use super::*;
use hermes_application::execution::*;
use hermes_domain::Identity;
use sqlx::Row;

pub(super) async fn verify(
    pool: &PgPool,
    store: &PgStore,
    scope: &InstallationScope,
    controller: &Controller,
    progress: &Api<ConfigMap>,
    bindings: &[(Uuid, String)],
) {
    let mutation = |key: String, command| ExecutionMutation {
        scope: scope.clone(),
        actor: Identity {
            issuer: scope.issuer.clone(),
            subject: "fixture-administrator".into(),
        },
        request_id: Uuid::new_v4(),
        idempotency_key: key,
        command,
    };
    let resources = WorkerResources {
        cpu_millis: 100,
        memory_mib: 256,
        ephemeral_storage_mib: 256,
    };
    for (id, image) in [
        (
            "python-tools",
            std::env::var("HERMES_WORKER_FIXTURE_IMAGE").unwrap(),
        ),
        (
            "shell-tools",
            std::env::var("HERMES_WORKER_FIXTURE_ALTERNATE_IMAGE").unwrap(),
        ),
    ] {
        store
            .mutate_execution(mutation(
                id.into(),
                ExecutionCommand::RegisterRuntime {
                    definition: RuntimeDefinition {
                        schema_version: 1,
                        runtime: RuntimeRef {
                            id: id.into(),
                            version: 1,
                        },
                        image,
                        ssh_contract: "managed-ssh-v2".into(),
                        uid: 1000,
                        gid: 1000,
                        service_account: "worker".into(),
                        resources: resources.clone(),
                        locale: "C.UTF-8".into(),
                        egress: vec![],
                        pull_secret_names: vec![],
                    },
                },
            ))
            .await
            .unwrap();
    }
    for (index, (instance, namespace)) in bindings.iter().enumerate() {
        let row = sqlx::query("SELECT a.id,a.version,i.namespace_uid FROM manager.assignments a JOIN manager.instances i ON i.assignment_id=a.id WHERE i.id=$1").bind(instance).fetch_one(pool).await.unwrap();
        let pvc_uid: String = sqlx::query_scalar("SELECT uid FROM manager.owned_resources WHERE instance_id=$1 AND kind='PersistentVolumeClaim' AND name='workspace'").bind(instance).fetch_one(pool).await.unwrap();
        let mount = PermittedMount {
            path: "/workspace".into(),
            read_only: false,
            sub_path: Some("projects/example".into()),
        };
        let binding = StorageBinding {
            id: Uuid::new_v4(),
            version: 1,
            instance_id: *instance,
            namespace: namespace.clone(),
            namespace_uid: row.get("namespace_uid"),
            pvc_name: "workspace".into(),
            pvc_uid,
            backing_storage_id: Uuid::new_v4(),
            external: false,
            required: true,
            backup: BackupPolicy::PlatformCheckpoint,
            mounts: vec![mount.clone()],
        };
        store
            .mutate_execution(mutation(
                format!("storage-{index}"),
                ExecutionCommand::RegisterStorage {
                    binding: binding.clone(),
                },
            ))
            .await
            .unwrap();
        let ids = if index == 0 {
            vec!["python-tools", "shell-tools"]
        } else {
            vec!["python-tools"]
        };
        store
            .mutate_execution(mutation(
                format!("policy-{index}"),
                ExecutionCommand::SetPolicy {
                    assignment_id: row.get("id"),
                    expected_version: row.get("version"),
                    policy: AssignmentExecution {
                        default_runtime_id: "python-tools".into(),
                        allowed: ids
                            .into_iter()
                            .map(|id| AllowedRuntime {
                                runtime: RuntimeRef {
                                    id: id.into(),
                                    version: 1,
                                },
                                resources: resources.clone(),
                                mounts: vec![MountSelection {
                                    binding_id: binding.id,
                                    binding_version: 1,
                                    mount: mount.clone(),
                                }],
                                limits: Default::default(),
                            })
                            .collect(),
                    },
                },
            ))
            .await
            .unwrap();
    }
    sqlx::query(
        "UPDATE manager.instances SET reconcile_after=clock_timestamp()-interval '1 second'",
    )
    .execute(pool)
    .await
    .unwrap();
    progress.patch("fixture-progress", &PatchParams::default(), &Patch::Merge(json!({"data":{"stage":"terminal","namespaceA":bindings[0].1,"namespaceB":bindings[1].1}}))).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        controller.step().await.unwrap();
        if progress
            .get("fixture-progress")
            .await
            .unwrap()
            .data
            .unwrap()["stage"]
            == "terminal-checked"
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native plugin SSH acceptance timed out"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        controller.step().await.unwrap();
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM manager.execution_environments WHERE state<>'Retired'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if live == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "released worker did not retire");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let states: (i64, i64) = sqlx::query_as("SELECT count(*),count(*) FILTER(WHERE host_key LIKE 'ssh-ed25519 %') FROM manager.execution_worker_ready_receipts").fetch_one(pool).await.unwrap();
    assert_eq!(states, (3, 3));
    let absences: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.execution_worker_absence")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(absences, 3);
    stage(progress, "terminal-retired", &bindings[0].1, &bindings[1].1).await;
}
