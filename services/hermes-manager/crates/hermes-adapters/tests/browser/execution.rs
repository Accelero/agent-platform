//! Execution administration inherits current administrator session and CSRF gates.
use super::*;

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn execution_registration_requires_csrf_and_current_administrator_role(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let (browser, session) = f.login().await;
    let identity: Value = f.session(&browser, &session).await.json().await.unwrap();
    let csrf = identity["csrfToken"].as_str().unwrap();
    // The same valid browser session remains an administration credential only.
    let (id, epoch, issuer): (Uuid, i64, String) =
        sqlx::query_as("SELECT id,epoch,issuer FROM manager.installations")
            .fetch_one(&pool)
            .await
            .unwrap();
    let store = PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let execution = start(
        &f.provider.secrets,
        hermes_adapters::execution_http::execution_router(
            store,
            InstallationScope { id, epoch, issuer },
        )
        .unwrap(),
        false,
    )
    .await;
    assert_eq!(
        f.client
            .get(format!("{}/execution/v1/environments", execution.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let runtime = json!({"schemaVersion":1,"runtime":{"id":"python-tools","version":1},
        "image":format!("ghcr.io/example/worker@sha256:{}","c".repeat(64)),
        "sshContract":"managed-ssh-v1","uid":1000,"gid":1000,"serviceAccount":"worker",
        "resources":{"cpuMillis":1000,"memoryMib":1024,"ephemeralStorageMib":1024},
        "locale":"C.UTF-8","egress":[],"pullSecretNames":[]});
    for (origin, proof, expected) in [
        (None, None, 403),
        (Some("https://foreign.example.org"), Some(csrf), 403),
        (Some("https://localhost"), Some("incorrect"), 403),
        (Some("https://localhost"), Some(csrf), 202),
    ] {
        let mut request = f
            .client
            .post(format!("{}/api/v1/terminal-runtimes", f.admin.url))
            .header(header::COOKIE, cookies(&browser, &session))
            .header("Idempotency-Key", "browser-runtime")
            .json(&runtime);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(proof) = proof {
            request = request.header("X-CSRF-Token", proof);
        }
        assert_eq!(request.send().await.unwrap().status(), expected);
    }
    let actor: String =
        sqlx::query_scalar("SELECT actor_subject FROM manager.execution_configuration_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(actor, "owner-a");
    sqlx::query("UPDATE manager_admin_auth.sessions SET verified_at=clock_timestamp()-interval '61 seconds' WHERE state='active'").execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE manager_admin_auth.administrator_grants SET active=false WHERE subject='owner-a'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let response = f
        .client
        .post(format!("{}/api/v1/terminal-runtimes", f.admin.url))
        .header(header::COOKIE, cookies(&browser, &session))
        .header(header::ORIGIN, "https://localhost")
        .header("X-CSRF-Token", csrf)
        .header("Idempotency-Key", "browser-runtime")
        .json(&runtime)
        .send()
        .await
        .unwrap();
    assert!(matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM manager.execution_configuration_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}
