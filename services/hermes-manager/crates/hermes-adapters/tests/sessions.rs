//! Session durability, browser-flow ordering and least-privilege SQL boundaries.
use std::collections::BTreeMap;

use hermes_adapters::{
    postgres::{FreshInventory, PgStore},
    session_crypto::{Purpose, Sealed, Sealer, hash},
    sessions::PgSessions,
};
use hermes_application::{Error, InstallationScope};
use hermes_domain::Identity;
use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn fixture(pool: &PgPool, purpose: Purpose) -> (PgSessions, InstallationScope) {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org/realms/agents".into(),
    };
    PgStore::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap()
        .initialize_fresh(
            &scope,
            FreshInventory {
                cluster_identity: "session-test".into(),
                digest: "a".repeat(64),
                registered_instances: 0,
                unexplained_resources: 0,
            },
        )
        .await
        .unwrap();
    (
        PgSessions::connect((*pool.connect_options()).clone(), scope.clone(), purpose)
            .await
            .unwrap(),
        scope,
    )
}
fn sealer() -> Sealer {
    Sealer::new(
        "key-one".into(),
        BTreeMap::from([("key-one".into(), vec![42; 32])]),
    )
    .unwrap()
}
fn material(scope: &InstallationScope, purpose: Purpose, kind: &str, id: &[u8]) -> Sealed {
    sealer()
        .seal(
            scope,
            purpose,
            kind,
            id,
            b"sensitive-provider-token-fixture",
        )
        .unwrap()
}
fn identity(scope: &InstallationScope) -> Identity {
    Identity {
        issuer: scope.issuer.clone(),
        subject: "owner-a".into(),
    }
}
async fn login(store: &PgSessions, browser: &[u8], state: &[u8], session: &[u8]) {
    let scope = store.scope();
    let generation = store
        .begin(
            browser,
            state,
            &material(scope, store.purpose(), "flow", state),
        )
        .await
        .unwrap();
    assert_eq!(
        store.consume(browser, state).await.unwrap().generation,
        generation
    );
    store
        .create(
            browser,
            generation,
            state,
            session,
            &identity(scope),
            &material(scope, store.purpose(), "session", session),
        )
        .await
        .unwrap();
}

#[test]
fn authenticated_encryption_binds_record_purpose_epoch_and_key_and_supports_rotation() {
    let scope = InstallationScope {
        id: Uuid::new_v4(),
        epoch: 1,
        issuer: "https://sso.example.org".into(),
    };
    let id = hash("session");
    let sealed = material(&scope, Purpose::Router, "session", &id);
    assert!(!sealed.ciphertext.windows(9).any(|p| p == b"sensitive"));
    let rotated = Sealer::new(
        "key-two".into(),
        BTreeMap::from([
            ("key-one".into(), vec![42; 32]),
            ("key-two".into(), vec![43; 32]),
        ]),
    )
    .unwrap();
    assert_eq!(
        &**rotated
            .open(&scope, Purpose::Router, "session", &id, &sealed)
            .unwrap(),
        b"sensitive-provider-token-fixture"
    );
    let mut newer = scope.clone();
    newer.epoch += 1;
    let mut other = scope.clone();
    other.id = Uuid::new_v4();
    for (scope, purpose, kind, id) in [
        (&newer, Purpose::Router, "session", id.clone()),
        (&other, Purpose::Router, "session", id.clone()),
        (&scope, Purpose::Administration, "session", id.clone()),
        (&scope, Purpose::Router, "flow", id.clone()),
        (&scope, Purpose::Router, "session", hash("other-session")),
    ] {
        assert!(rotated.open(scope, purpose, kind, &id, &sealed).is_err());
    }
    let mut tampered = sealed.clone();
    tampered.ciphertext[0] ^= 1;
    assert!(
        rotated
            .open(&scope, Purpose::Router, "session", &id, &tampered)
            .is_err()
    );
    let retired = Sealer::new(
        "key-two".into(),
        BTreeMap::from([("key-two".into(), vec![43; 32])]),
    )
    .unwrap();
    assert!(
        retired
            .open(&scope, Purpose::Router, "session", &id, &sealed)
            .is_err()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn flows_are_browser_bound_single_use_and_session_creation_is_single_use(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser-a");
    let state = hash("state");
    let session = hash("session");
    let sealed = material(&scope, Purpose::Router, "flow", &state);
    let generation = store.begin(&browser, &state, &sealed).await.unwrap();
    assert!(store.consume(&hash("browser-b"), &state).await.is_err());
    assert!(store.consume(&browser, &hash("wrong-state")).await.is_err());
    assert_eq!(
        store.consume(&browser, &state).await.unwrap().generation,
        generation
    );
    assert!(store.consume(&browser, &state).await.is_err());
    let value = material(&scope, Purpose::Router, "session", &session);
    store
        .create(
            &browser,
            generation,
            &state,
            &session,
            &identity(&scope),
            &value,
        )
        .await
        .unwrap();
    assert!(
        store
            .create(
                &browser,
                generation,
                &state,
                &hash("other"),
                &identity(&scope),
                &value
            )
            .await
            .is_err()
    );
    assert!(
        store
            .load(&session, Some(&hash("browser-b")))
            .await
            .is_err()
    );
    let loaded = store.load(&session, Some(&browser)).await.unwrap();
    assert_eq!(loaded.identity, identity(&scope));
    assert!(!loaded.refresh_due);
    assert_eq!(loaded.sealed.ciphertext, value.ciphertext);
    let persisted: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM manager_router_auth.sessions WHERE id_hash=$1")
            .bind(&session)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(persisted, b"sensitive-provider-token-fixture");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn overlapping_login_and_logout_fence_pending_callbacks_and_existing_sessions(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    login(&store, &browser, &hash("original"), &session).await;
    let other_browser = hash("other-browser");
    let other_session = hash("other-session");
    login(&store, &other_browser, &hash("other-state"), &other_session).await;
    let state = hash("in-flight");
    let generation = store
        .begin(
            &browser,
            &state,
            &material(&scope, Purpose::Router, "flow", &state),
        )
        .await
        .unwrap();
    store.consume(&browser, &state).await.unwrap();
    assert!(store.load(&session, None).await.is_err());
    let newer = hash("newer-flow");
    assert!(
        store
            .begin(
                &browser,
                &newer,
                &material(&scope, Purpose::Router, "flow", &newer)
            )
            .await
            .unwrap()
            > generation
    );
    assert!(
        store
            .create(
                &browser,
                generation,
                &state,
                &hash("late-session"),
                &identity(&scope),
                &material(&scope, Purpose::Router, "session", &session)
            )
            .await
            .is_err()
    );
    store.logout(&browser).await.unwrap();
    assert!(store.consume(&browser, &newer).await.is_err());
    assert!(
        store
            .load(&other_session, Some(&other_browser))
            .await
            .is_ok()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn concurrent_refresh_is_exclusive_and_finish_never_extends_absolute_expiry(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    login(&store, &browser, &hash("state"), &session).await;
    sqlx::query("UPDATE manager_router_auth.sessions SET verified_at=clock_timestamp()-interval '61 seconds'").execute(&pool).await.unwrap();
    assert!(store.load(&session, None).await.unwrap().refresh_due);
    let expiry: String =
        sqlx::query_scalar("SELECT expires_at::text FROM manager_router_auth.sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let store = store.clone();
        let session = session.clone();
        jobs.spawn(async move { store.claim_refresh(&session, None).await });
    }
    let mut claims = vec![];
    while let Some(result) = jobs.join_next().await {
        match result.unwrap() {
            Ok(claim) => claims.push(claim),
            Err(e) => assert_eq!(e, Error::Unavailable),
        }
    }
    assert_eq!(claims.len(), 1);
    assert!(matches!(
        store.load(&session, None).await,
        Err(Error::Unavailable)
    ));
    let claim = claims.pop().unwrap();
    let value = material(&scope, Purpose::Router, "session", &session);
    let mut foreign = identity(&scope);
    foreign.subject = "owner-b".into();
    assert!(
        store
            .finish_refresh(&claim, &foreign, &value)
            .await
            .is_err()
    );
    store
        .finish_refresh(&claim, &identity(&scope), &value)
        .await
        .unwrap();
    assert!(
        store
            .finish_refresh(&claim, &identity(&scope), &value)
            .await
            .is_err()
    );
    assert!(!store.load(&session, None).await.unwrap().refresh_due);
    let after: String =
        sqlx::query_scalar("SELECT expires_at::text FROM manager_router_auth.sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(after, expiry);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn crash_during_refresh_revokes_on_reopen_instead_of_replaying_rotated_token(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    login(&store, &browser, &hash("state"), &session).await;
    let claim = store.claim_refresh(&session, None).await.unwrap();
    sqlx::query("UPDATE manager_router_auth.sessions SET refresh_until=clock_timestamp()-interval '1 second'").execute(&pool).await.unwrap();
    drop(store);
    let reopened = PgSessions::connect(
        (*pool.connect_options()).clone(),
        scope.clone(),
        Purpose::Router,
    )
    .await
    .unwrap();
    assert!(matches!(
        reopened.load(&session, None).await,
        Err(Error::InvalidIdentityEvidence)
    ));
    assert!(reopened.claim_refresh(&session, None).await.is_err());
    assert!(
        reopened
            .finish_refresh(
                &claim,
                &identity(&scope),
                &material(&scope, Purpose::Router, "session", &session)
            )
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM manager_router_auth.sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "revoked");
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn logout_or_identity_switch_during_refresh_prevents_stale_result_commit(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    login(&store, &browser, &hash("state"), &session).await;
    let claim = store.claim_refresh(&session, None).await.unwrap();
    // Logout can invalidate even a session whose external refresh is in flight.
    store.load_for_logout(&session, &browser).await.unwrap();
    store.logout(&browser).await.unwrap();
    assert!(
        store
            .finish_refresh(
                &claim,
                &identity(&scope),
                &material(&scope, Purpose::Router, "session", &session)
            )
            .await
            .is_err()
    );
    assert!(store.load(&session, None).await.is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn epoch_and_maintenance_mirrors_fence_router_but_allow_admin_recovery_login(pool: PgPool) {
    let (router, scope) = fixture(&pool, Purpose::Router).await;
    let admin = PgSessions::connect(
        (*pool.connect_options()).clone(),
        scope.clone(),
        Purpose::Administration,
    )
    .await
    .unwrap();
    let browser = hash("browser");
    let session = hash("session");
    login(&router, &browser, &hash("state"), &session).await;
    sqlx::query("UPDATE manager.installations SET mode='maintenance'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        router.load(&session, None).await,
        Err(Error::Maintenance)
    ));
    router.load_for_logout(&session, &browser).await.unwrap();
    login(&admin, &browser, &hash("admin-state"), &session).await;
    assert!(admin.load(&session, None).await.is_ok());
    sqlx::query("UPDATE manager.installations SET epoch=epoch+1,mode='restore'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(router.check().await, Err(Error::RecoveryRequired));
    assert_eq!(admin.check().await, Err(Error::RecoveryRequired));
    let mut next = scope.clone();
    next.epoch += 1;
    let restored = PgSessions::connect(
        (*pool.connect_options()).clone(),
        next,
        Purpose::Administration,
    )
    .await
    .unwrap();
    assert!(restored.load(&session, None).await.is_err());
    assert!(
        sqlx::query("UPDATE manager.installations SET epoch=1")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE manager.installations SET issuer='https://foreign.example.org'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE manager.installations SET id=$1")
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn expired_browser_flow_or_session_cannot_authenticate(pool: PgPool) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    let state = hash("state");
    store
        .begin(
            &browser,
            &state,
            &material(&scope, Purpose::Router, "flow", &state),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE manager_router_auth.flows SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(store.consume(&browser, &state).await.is_err());
    login(&store, &browser, &hash("fresh-state"), &session).await;
    sqlx::query(
        "UPDATE manager_router_auth.sessions SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(store.load(&session, None).await.is_err());
    login(&store, &browser, &hash("new-state"), &hash("new-session")).await;
    sqlx::query(
        "UPDATE manager_router_auth.browsers SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(store.load(&hash("new-session"), None).await.is_err());
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn router_session_role_has_no_assignment_other_session_or_epoch_write_rights(pool: PgPool) {
    let (_, scope) = fixture(&pool, Purpose::Router).await;
    let role = format!("auth_test_{}", Uuid::new_v4().simple());
    // Generated identifier contains only a fixed prefix and UUID hex; no inputs.
    let ddl = format!(
        "CREATE ROLE {role} LOGIN; GRANT CONNECT ON DATABASE \"{}\" TO {role}; GRANT USAGE ON SCHEMA manager_router_auth TO {role}; GRANT SELECT,INSERT,UPDATE,DELETE ON manager_router_auth.browsers,manager_router_auth.flows,manager_router_auth.sessions TO {role}; GRANT EXECUTE ON FUNCTION manager_router_auth.check_scope(uuid,bigint,text) TO {role};",
        pool.connect_options().get_database().unwrap()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
        .execute(&pool)
        .await
        .unwrap();
    let options = (*pool.connect_options()).clone().username(&role);
    let limited = PgPool::connect_with(options.clone()).await.unwrap();
    for query in [
        "SELECT * FROM manager.assignments",
        "SELECT * FROM manager_admin_auth.sessions",
        "UPDATE manager_router_auth.scope SET epoch=epoch+1",
        "SELECT * FROM manager_router_auth.scope",
    ] {
        let err = sqlx::query(query).execute(&limited).await.unwrap_err();
        assert_eq!(
            err.as_database_error().unwrap().code().as_deref(),
            Some("42501")
        );
    }
    let sessions = PgSessions::connect(options, scope, Purpose::Router)
        .await
        .unwrap();
    let browser = hash("browser");
    let session = hash("session");
    login(&sessions, &browser, &hash("state"), &session).await;
    assert!(sessions.load(&session, None).await.is_ok());
    sessions.logout(&browser).await.unwrap();
    assert!(sessions.load(&session, None).await.is_err());
    drop(sessions);
    limited.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY {role}; DROP ROLE {role};"
    )))
    .execute(&pool)
    .await
    .unwrap();
    let row = sqlx::query("SELECT epoch FROM manager_router_auth.scope")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("epoch"), 1);
}

#[sqlx::test(migrator = "hermes_adapters::postgres::MIGRATOR")]
async fn credential_cleanup_is_bounded_and_preserves_live_sessions_and_inflight_logins(
    pool: PgPool,
) {
    let (store, scope) = fixture(&pool, Purpose::Router).await;
    let browser = hash("browser");
    let session = hash("session");
    login(&store, &browser, &hash("state"), &session).await;
    let pending_browser = hash("pending-browser");
    let pending_state = hash("pending");
    store
        .begin(
            &pending_browser,
            &pending_state,
            &material(&scope, Purpose::Router, "flow", &pending_state),
        )
        .await
        .unwrap();
    for i in 0..5 {
        login(
            &store,
            &hash(&format!("old-browser-{i}")),
            &hash(&format!("old-state-{i}")),
            &hash(&format!("old-session-{i}")),
        )
        .await;
    }
    sqlx::query("UPDATE manager_router_auth.sessions SET state='revoked' WHERE id_hash<>$1")
        .bind(&session)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(store.prune(2).await.unwrap(), 2);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager_router_auth.sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 4);
    store.prune(100).await.unwrap();
    assert!(store.load(&session, None).await.is_ok());
    assert!(
        store
            .consume(&pending_browser, &pending_state)
            .await
            .is_ok()
    );
    assert_eq!(store.prune(0).await, Err(Error::Invalid));
    assert_eq!(store.prune(1001).await, Err(Error::Invalid));
}
