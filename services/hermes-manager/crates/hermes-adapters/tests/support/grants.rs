//! Explicit synthetic grants for pre-existing integration fixtures.
pub async fn grant(
    pool: &sqlx::PgPool,
    scope: &hermes_application::InstallationScope,
    subjects: &[&str],
) {
    for subject in subjects {
        sqlx::query("INSERT INTO manager_admin_auth.administrator_grants(id,installation_id,issuer,subject,active,can_manage_administrators) VALUES($1,$2,$3,$4,true,true) ON CONFLICT DO NOTHING").bind(uuid::Uuid::new_v4()).bind(scope.id).bind(&scope.issuer).bind(subject).execute(pool).await.unwrap();
    }
}
