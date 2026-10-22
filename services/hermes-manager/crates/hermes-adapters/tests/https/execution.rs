//! Real HTTPS administration of reference-only execution configuration.
use super::*;

fn runtime(id: &str, version: i64) -> Value {
    json!({"schemaVersion":1,"runtime":{"id":id,"version":version},
        "image":format!("ghcr.io/example/worker@sha256:{}","c".repeat(64)),
        "sshContract":"managed-ssh-v1","uid":1000,"gid":1000,"serviceAccount":"worker",
        "resources":{"cpuMillis":1000,"memoryMib":1024,"ephemeralStorageMib":1024},
        "locale":"C.UTF-8","egress":[],"pullSecretNames":[]})
}
async fn post(f: &Fixture, path: &str, key: &str, body: &Value) -> reqwest::Response {
    f.client
        .post(format!("{}{path}", f.admin.url))
        .bearer_auth(f.admin_token())
        .header("Idempotency-Key", key)
        .json(body)
        .send()
        .await
        .unwrap()
}
async fn owner(f: &Fixture, pool: &PgPool, subject: &str) -> (Value, String, String) {
    let created = f.create(subject).await;
    let instance = Uuid::parse_str(created["instanceId"].as_str().unwrap()).unwrap();
    let namespace = hermes_domain::instance_namespace(f.scope.id, instance);
    let uid = Uuid::new_v4().to_string();
    sqlx::query("UPDATE manager.instances SET namespace_uid=$1 WHERE id=$2")
        .bind(&uid)
        .bind(instance)
        .execute(pool)
        .await
        .unwrap();
    (created, namespace, uid)
}
fn storage(created: &Value, namespace: &str, uid: &str) -> Value {
    json!({"id":Uuid::new_v4(),"version":1,"instanceId":created["instanceId"],
        "namespace":namespace,"namespaceUid":uid,"pvcName":"network-home","pvcUid":Uuid::new_v4(),
        "backingStorageId":Uuid::new_v4(),"external":true,"required":true,"backup":"external",
        "mounts":[{"path":"/mnt/network-home","readOnly":true,"subPath":"documents"}]})
}
fn policy(binding: &Value) -> Value {
    json!({"defaultRuntimeId":"python-tools","allowed":[{
        "runtime":{"id":"python-tools","version":1},
        "resources":{"cpuMillis":1000,"memoryMib":1024,"ephemeralStorageMib":1024},
        "mounts":[{"bindingId":binding["id"],"bindingVersion":binding["version"],"mount":binding["mounts"][0]}]
    }]})
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn catalog_storage_and_desired_policy_are_audited_isolated_and_pending(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let r = runtime("python-tools", 1);
    let response = post(&f, "/api/v1/terminal-runtimes", "runtime-register", &r).await;
    assert_eq!(response.status(), 202);
    let first: Value = response.json().await.unwrap();
    assert_eq!(
        post(&f, "/api/v1/terminal-runtimes", "runtime-register", &r)
            .await
            .json::<Value>()
            .await
            .unwrap(),
        first
    );
    let mut changed = r.clone();
    changed["locale"] = json!("en_US.UTF-8");
    assert_eq!(
        post(
            &f,
            "/api/v1/terminal-runtimes",
            "runtime-register",
            &changed
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        post(
            &f,
            "/api/v1/terminal-runtimes/other/versions",
            "wrong-path",
            &runtime("python-tools", 2)
        )
        .await
        .status(),
        422
    );
    assert_eq!(
        post(
            &f,
            "/api/v1/terminal-runtimes/python-tools/versions",
            "runtime-v2",
            &runtime("python-tools", 2)
        )
        .await
        .status(),
        202
    );
    let page: Value = f
        .client
        .get(format!(
            "{}/api/v1/terminal-runtimes/python-tools/versions?limit=1",
            f.admin.url
        ))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["items"][0], r);
    assert_eq!(page["nextCursor"], 1);
    let page: Value = f
        .client
        .get(format!(
            "{}/api/v1/terminal-runtimes/python-tools/versions?after=1&limit=1",
            f.admin.url
        ))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["items"][0]["runtime"]["version"], 2);
    assert_eq!(
        post(
            &f,
            "/api/v1/terminal-runtimes",
            "rust-runtime",
            &runtime("rust-tools", 1)
        )
        .await
        .status(),
        202
    );
    let mut cursor = String::new();
    for expected in ["python-tools", "rust-tools"] {
        let page: Value = f
            .client
            .get(format!(
                "{}/api/v1/terminal-runtimes?limit=1{cursor}",
                f.admin.url
            ))
            .bearer_auth(f.admin_token())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        cursor = page["nextCursor"]
            .as_str()
            .map(|value| format!("&after={value}"))
            .unwrap_or_default();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["runtime"]["id"], expected);
        if expected == "python-tools" {
            assert_eq!(page["items"][0]["runtime"]["version"], 2);
        }
    }
    let (a, namespace_a, uid_a) = owner(&f, &pool, "execution-owner-a").await;
    let (b, namespace_b, uid_b) = owner(&f, &pool, "execution-owner-b").await;
    let binding = storage(&a, &namespace_a, &uid_a);
    assert_eq!(
        post(&f, "/api/v1/storage-bindings", "storage-a", &binding)
            .await
            .status(),
        202
    );
    let own_list: Value = f
        .client
        .get(format!(
            "{}/api/v1/storage-bindings?instanceId={}",
            f.admin.url,
            a["instanceId"].as_str().unwrap()
        ))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(own_list["items"], json!([binding]));
    let other_list: Value = f
        .client
        .get(format!(
            "{}/api/v1/storage-bindings?instanceId={}",
            f.admin.url,
            b["instanceId"].as_str().unwrap()
        ))
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other_list["items"], json!([]));
    let mut stolen = binding.clone();
    stolen["instanceId"] = b["instanceId"].clone();
    stolen["namespace"] = json!(namespace_b);
    stolen["namespaceUid"] = json!(uid_b);
    assert!(
        !post(&f, "/api/v1/storage-bindings", "stolen-id", &stolen)
            .await
            .status()
            .is_success()
    );
    let policy_a = format!(
        "{}/api/v1/assignments/{}/execution",
        f.admin.url,
        a["assignmentId"].as_str().unwrap()
    );
    let policy_b = format!(
        "{}/api/v1/assignments/{}/execution",
        f.admin.url,
        b["assignmentId"].as_str().unwrap()
    );
    let p = policy(&binding);
    let other = f
        .client
        .patch(&policy_b)
        .bearer_auth(f.admin_token())
        .header("Idempotency-Key", "cross-owner-policy")
        .header("If-Match", "\"1\"")
        .json(&p)
        .send()
        .await
        .unwrap();
    assert!(!other.status().is_success());
    let update = || {
        f.client
            .patch(&policy_a)
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "policy-a")
            .header("If-Match", "\"1\"")
            .json(&p)
    };
    let result = update().send().await.unwrap();
    assert_eq!(result.status(), 202);
    assert_eq!(result.headers()["etag"], "\"2\"");
    let accepted: Value = result.json().await.unwrap();
    assert_eq!(
        update()
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        f.client
            .patch(&policy_a)
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "stale-policy")
            .header("If-Match", "\"1\"")
            .json(&p)
            .send()
            .await
            .unwrap()
            .status(),
        412
    );
    let view = f
        .client
        .get(&policy_a)
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(view.headers()["etag"], "\"2\"");
    assert_eq!(view.headers()["cache-control"], "no-store");
    let view: Value = view.json().await.unwrap();
    assert_eq!(view["desiredGeneration"], 1);
    assert_eq!(view["appliedGeneration"], 0);
    assert_eq!(view["state"], "Pending");
    assert_eq!(
        view["desired"]["runtimes"][0]["mounts"][0]["binding"],
        binding
    );
    let storage_url = format!(
        "{}/api/v1/storage-bindings/{}",
        f.admin.url,
        binding["id"].as_str().unwrap()
    );
    let current = f
        .client
        .get(&storage_url)
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap();
    assert_eq!(current.headers()["etag"], "\"1\"");
    assert_eq!(current.json::<Value>().await.unwrap(), binding);
    let mut next = binding.clone();
    next["version"] = json!(2);
    next["required"] = json!(false);
    let update_storage = || {
        f.client
            .patch(&storage_url)
            .bearer_auth(f.admin_token())
            .header("If-Match", "\"1\"")
            .header("Idempotency-Key", "storage-version-2")
            .json(&next)
    };
    let updated = update_storage().send().await.unwrap();
    assert_eq!(updated.status(), 202);
    assert_eq!(updated.headers()["etag"], "\"2\"");
    let accepted: Value = updated.json().await.unwrap();
    assert_eq!(
        update_storage()
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        f.client
            .patch(&storage_url)
            .bearer_auth(f.admin_token())
            .header("If-Match", "\"1\"")
            .header("Idempotency-Key", "stale-storage")
            .json(&next)
            .send()
            .await
            .unwrap()
            .status(),
        412
    );
    assert_eq!(
        f.client
            .get(&storage_url)
            .bearer_auth(f.admin_token())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        next
    );
    let view: Value = f
        .client
        .get(&policy_a)
        .bearer_auth(f.admin_token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        view["desired"]["runtimes"][0]["mounts"][0]["binding"],
        binding
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM manager.execution_configuration_events WHERE action LIKE 'execution.%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 6);
    let routes: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.instance_routes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(routes, 0);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn execution_administration_rejects_credentials_raw_specs_and_ambiguous_input(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (created, namespace, uid) = owner(&f, &pool, "execution-input-owner").await;
    let policy_path = format!(
        "/api/v1/assignments/{}/execution",
        created["assignmentId"].as_str().unwrap()
    );
    let pull_path = format!(
        "/api/v1/instances/{}/pull-secret-bindings",
        created["instanceId"].as_str().unwrap()
    );
    for path in [
        "/api/v1/terminal-runtimes",
        "/api/v1/terminal-runtimes/python-tools/versions",
        "/api/v1/storage-bindings",
        &policy_path,
    ] {
        for token in [
            None,
            Some("opaque-native-client-secret".into()),
            Some(f.route_token("execution-input-owner")),
        ] {
            let request = f.client.get(format!("{}{path}", f.admin.url));
            let response = match token {
                Some(token) => request.bearer_auth(token),
                None => request,
            }
            .send()
            .await
            .unwrap();
            assert_eq!(response.status(), 401);
        }
        let token = f.sign(&f.claims(
            "execution-input-owner",
            "manager-api",
            "manager-admin",
            false,
        ));
        assert_eq!(
            f.client
                .get(format!("{}{path}", f.admin.url))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    for (token, expected) in [
        ("opaque-native-client-secret".into(), 401),
        (f.route_token("execution-input-owner"), 401),
        (
            f.sign(&f.claims(
                "execution-input-owner",
                "manager-api",
                "manager-admin",
                false,
            )),
            403,
        ),
    ] {
        for (path, body) in [
            ("/api/v1/terminal-runtimes", runtime("denied-worker", 1)),
            (
                "/api/v1/storage-bindings",
                storage(&created, &namespace, &uid),
            ),
            (&pull_path, json!({})),
        ] {
            assert_eq!(
                f.client
                    .post(format!("{}{path}", f.admin.url))
                    .bearer_auth(&token)
                    .header("Idempotency-Key", "denied-write")
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
    }
    let mut raw = runtime("python-tools", 1);
    raw["podSpec"] = json!({"hostNetwork":true});
    assert_eq!(
        post(&f, "/api/v1/terminal-runtimes", "raw-spec", &raw)
            .await
            .status(),
        422
    );
    let r = runtime("python-tools", 1).to_string();
    let duplicate = r.replacen("\"uid\":1000", "\"uid\":1000,\"uid\":0", 1);
    assert_eq!(
        f.client
            .post(format!("{}/api/v1/terminal-runtimes", f.admin.url))
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "duplicate")
            .header("Content-Type", "application/json")
            .body(duplicate)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    for query in [
        "limit=0",
        "limit=101",
        "limit=1&limit=2",
        "after=-1",
        "unknown=1",
    ] {
        assert_eq!(
            f.client
                .get(format!(
                    "{}/api/v1/terminal-runtimes/python-tools/versions?{query}",
                    f.admin.url
                ))
                .bearer_auth(f.admin_token())
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    let mut pull = json!({"instanceId":created["instanceId"],"namespace":namespace,"namespaceUid":uid,"name":"worker-pull","uid":Uuid::new_v4()});
    pull["token"] = json!("inline-secret-forbidden");
    assert_eq!(
        post(&f, &pull_path, "raw-secret", &pull).await.status(),
        422
    );
    pull.as_object_mut().unwrap().remove("token");
    assert_eq!(post(&f, &pull_path, "pull-ref", &pull).await.status(), 202);
    pull["instanceId"] = json!(Uuid::new_v4());
    assert_eq!(
        post(&f, &pull_path, "wrong-owner-pull", &pull)
            .await
            .status(),
        422
    );
    let body = json!({"defaultRuntimeId":"python-tools","allowed":[]});
    for etag in [None, Some("1"), Some("\"01\""), Some("*")] {
        let request = f
            .client
            .patch(format!("{}{policy_path}", f.admin.url))
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "invalid-etag")
            .json(&body);
        let response = match etag {
            Some(etag) => request.header("If-Match", etag),
            None => request,
        }
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 422);
    }
    let oversized = format!("{{\"ignored\":\"{}\"}}", "x".repeat(16 * 1024));
    assert_eq!(
        f.client
            .post(format!("{}/api/v1/terminal-runtimes", f.admin.url))
            .bearer_auth(f.admin_token())
            .header("Idempotency-Key", "oversized")
            .header("Content-Type", "application/json")
            .body(oversized)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
}
