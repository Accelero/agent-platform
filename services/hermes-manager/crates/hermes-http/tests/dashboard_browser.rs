//! The built dashboard in a real browser under the real authentication, CSP
//! and PostgreSQL grants. Needs `MANAGER_UI_DIST` (the `ui/dist` directory),
//! the UI's installed `node_modules` and Playwright's Chromium, so it is
//! ignored by default:
//!
//! ```text
//! MANAGER_UI_DIST="$PWD/ui/dist" DATABASE_URL=… \
//!   cargo test -p hermes-http --test dashboard_browser -- --ignored
//! ```

#[allow(dead_code)]
mod support;

use std::time::Duration;

use sqlx::PgPool;
use support::Fixture;

#[ignore = "needs the built UI and a Playwright browser"]
#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn production_dashboard_browser_csp(pool: PgPool) {
    let assets = std::env::var("MANAGER_UI_DIST").expect("MANAGER_UI_DIST");
    let fixture = Fixture::serving(pool, std::path::Path::new(&assets)).await;
    let cookies = fixture.browser_login().await.expect("administrator login");
    let value = |name: &str| {
        cookies
            .split("; ")
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .expect("cookie")
            .to_owned()
    };
    let ui = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui");
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(ui)
        .args([
            "node_modules/@playwright/test/cli.js",
            "test",
            "--config",
            "playwright.production.config.ts",
        ])
        .env("MANAGER_E2E_ORIGIN", &fixture.url)
        .env("MANAGER_E2E_SESSION", value("__Host-hermes-admin"))
        .env("MANAGER_E2E_BROWSER", value("__Host-hermes-admin-browser"))
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(180), command.output())
        .await
        .expect("browser run finishes")
        .expect("node runs");
    assert!(
        output.status.success(),
        "production browser failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
