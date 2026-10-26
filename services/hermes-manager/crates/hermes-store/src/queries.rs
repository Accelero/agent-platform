//! The administration read side: keyset pages over static SQL. Optional
//! filters are SQL parameters that may be null; nothing is concatenated.

use async_trait::async_trait;
use hermes_core::app::catalog::Definition;
use hermes_core::app::error::StoreError;
use hermes_core::app::queries::{
    AdminQueries, AdministratorView, AssignmentFilter, AssignmentSummary, AssignmentView,
    AuditActor, AuditFilter, AuditTargetView, AuditView, CatalogSummary, Condition,
    EnvironmentAdminView, IdentitySummary, InstanceView, OrphanReport, OrphanView, Page,
    PageRequest, Phase, SecretSummary, VersionSummary, VersionView, VolumeUse,
};
use hermes_core::domain::assignments::{Access, LifetimeMode};
use hermes_core::domain::catalog::{CatalogKind, VersionRef, VersionState};
use hermes_core::domain::execution::{EnvironmentState, WorkerFailure};
use hermes_core::domain::identity::Identity;
use hermes_core::domain::orphans::OrphanKind;
use hermes_core::domain::secrets::SecretKind;
use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::types::Json;
use uuid::Uuid;

use crate::{PgStore, store_error};

/// Fetches one more row than the page size to know whether a next page exists.
fn page<T>(mut items: Vec<T>, page: &PageRequest, cursor: impl Fn(&T) -> String) -> Page<T> {
    let more = items.len() > page.limit as usize;
    items.truncate(page.limit as usize);
    let next_cursor = if more { items.last().map(cursor) } else { None };
    Page { items, next_cursor }
}

fn fetch_limit(page: &PageRequest) -> i64 {
    i64::from(page.limit) + 1
}

fn identity(row: &PgRow) -> Identity {
    Identity {
        issuer: row.get("issuer"),
        subject: row.get("subject"),
    }
}

fn identity_summary(row: &PgRow) -> IdentitySummary {
    IdentitySummary {
        id: row.get("id"),
        identity: identity(row),
        display_name: row.get("display_name"),
        email: row.get("email"),
        verified: row.get("verified"),
        assignment_id: row.get("assignment_id"),
        administrator: row.get("administrator"),
    }
}

fn failure(row: &PgRow) -> Result<Option<WorkerFailure>, StoreError> {
    row.get::<Option<String>, _>("failure")
        .map(|value| WorkerFailure::parse(&value).ok_or(StoreError::Unavailable))
        .transpose()
}

fn phase(row: &PgRow) -> Result<Phase, StoreError> {
    Phase::parse(row.get("phase")).ok_or(StoreError::Unavailable)
}

fn instance(row: &PgRow) -> Result<InstanceView, StoreError> {
    let Json(conditions) = row
        .try_get::<Json<Vec<Condition>>, _>("conditions")
        .map_err(|_| StoreError::Unavailable)?;
    Ok(InstanceView {
        id: row.get("id"),
        assignment_id: row.get("assignment_id"),
        namespace: row.get("namespace"),
        desired_generation: row.get("desired_generation"),
        observed_generation: row.get("observed_generation"),
        execution_generation: row.get("execution_generation"),
        phase: phase(row)?,
        blocked_reason: row.get("blocked_reason"),
        conditions,
        observed_at: row.get("observed_at"),
    })
}

#[async_trait]
impl AdminQueries for PgStore {
    async fn environments(
        &self,
        instance: Uuid,
    ) -> Result<Option<Vec<EnvironmentAdminView>>, StoreError> {
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM manager.instances WHERE id = $1)")
                .bind(instance)
                .fetch_one(&self.pool)
                .await
                .map_err(store_error)?;
        if !known {
            return Ok(None);
        }
        let rows = sqlx::query(
            "SELECT e.id, e.backend_id, e.backend_version, e.execution_generation, e.mode, e.key, e.state, e.failure, \
             (SELECT count(*) FROM manager.leases l WHERE l.environment_id = e.id AND l.state = 'active' \
                AND l.expires_at > clock_timestamp()) AS leases, \
             to_char(e.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS created_at \
             FROM manager.environments e WHERE e.instance_id = $1 ORDER BY e.created_at, e.id LIMIT 256",
        )
        .bind(instance)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter()
            .map(|row| {
                let key: String = row.get("key");
                Ok(EnvironmentAdminView {
                    id: row.get("id"),
                    backend: VersionRef {
                        id: row.get("backend_id"),
                        version: row.get("backend_version"),
                    },
                    mode: LifetimeMode::parse(row.get("mode")).ok_or(StoreError::Unavailable)?,
                    session: key.strip_prefix("session:").map(str::to_owned),
                    generation: row.get("execution_generation"),
                    state: EnvironmentState::parse(row.get("state"))
                        .ok_or(StoreError::Unavailable)?,
                    failure: failure(row)?.map(WorkerFailure::as_str),
                    leases: row.get("leases"),
                    created_at: row.get("created_at"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()
            .map(Some)
    }

    async fn orphans(&self) -> Result<OrphanReport, StoreError> {
        let scanned_at: Option<String> = sqlx::query_scalar(
            "SELECT to_char(scanned_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') \
             FROM manager.orphan_scan",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        let rows = sqlx::query(
            "SELECT kind, name, uid, \
             to_char(first_seen_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS first_seen_at, \
             to_char(last_seen_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS last_seen_at \
             FROM manager.orphans ORDER BY kind, name LIMIT 500",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(OrphanView {
                    kind: OrphanKind::parse(row.get("kind"))
                        .ok_or(StoreError::Unavailable)?
                        .as_str(),
                    name: row.get("name"),
                    uid: row.get("uid"),
                    first_seen_at: row.get("first_seen_at"),
                    last_seen_at: row.get("last_seen_at"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(OrphanReport { scanned_at, items })
    }

    async fn identities(
        &self,
        q: Option<&str>,
        request: &PageRequest,
    ) -> Result<Page<IdentitySummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT i.id, i.issuer, i.subject, i.display_name, i.email, i.verified_at IS NOT NULL AS verified, \
             a.id AS assignment_id, coalesce(g.active, false) AS administrator \
             FROM manager.identities i LEFT JOIN manager.assignments a ON a.identity_id = i.id \
             LEFT JOIN manager.administrator_grants g ON g.identity_id = i.id \
             WHERE ($1::text IS NULL OR i.subject > $1) \
             AND ($2::text IS NULL OR strpos(lower(i.subject), lower($2)) > 0 \
                  OR strpos(lower(coalesce(i.email, '')), lower($2)) > 0 \
                  OR strpos(lower(coalesce(i.display_name, '')), lower($2)) > 0) \
             ORDER BY i.subject LIMIT $3",
        )
        .bind(&request.after)
        .bind(q)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows.iter().map(identity_summary).collect();
        Ok(page(items, request, |item: &IdentitySummary| {
            item.identity.subject.clone()
        }))
    }

    async fn identity(&self, id: Uuid) -> Result<Option<IdentitySummary>, StoreError> {
        let row = sqlx::query(
            "SELECT i.id, i.issuer, i.subject, i.display_name, i.email, i.verified_at IS NOT NULL AS verified, \
             a.id AS assignment_id, coalesce(g.active, false) AS administrator \
             FROM manager.identities i LEFT JOIN manager.assignments a ON a.identity_id = i.id \
             LEFT JOIN manager.administrator_grants g ON g.identity_id = i.id WHERE i.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(row.as_ref().map(identity_summary))
    }

    async fn administrator_candidates(
        &self,
        q: Option<&str>,
        request: &PageRequest,
    ) -> Result<Page<IdentitySummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT i.id, i.issuer, i.subject, i.display_name, i.email, true AS verified, \
             a.id AS assignment_id, false AS administrator \
             FROM manager.identities i LEFT JOIN manager.assignments a ON a.identity_id = i.id \
             WHERE i.verified_at IS NOT NULL \
             AND NOT EXISTS (SELECT 1 FROM manager.administrator_grants g WHERE g.identity_id = i.id) \
             AND ($1::text IS NULL OR i.subject > $1) \
             AND ($2::text IS NULL OR strpos(lower(i.subject), lower($2)) > 0 \
                  OR strpos(lower(coalesce(i.email, '')), lower($2)) > 0) \
             ORDER BY i.subject LIMIT $3",
        )
        .bind(&request.after)
        .bind(q)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows.iter().map(identity_summary).collect();
        Ok(page(items, request, |item: &IdentitySummary| {
            item.identity.subject.clone()
        }))
    }

    async fn assignments(
        &self,
        filter: &AssignmentFilter,
        request: &PageRequest,
    ) -> Result<Page<AssignmentSummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT a.id, i.issuer, i.subject, a.access, a.template_id, a.template_version, a.version, \
             n.id AS instance_id, n.phase FROM manager.assignments a \
             JOIN manager.identities i ON i.id = a.identity_id JOIN manager.instances n ON n.assignment_id = a.id \
             WHERE ($1::text IS NULL OR i.subject > $1) \
             AND ($2::text IS NULL OR strpos(lower(i.subject), lower($2)) > 0 \
                  OR strpos(lower(coalesce(i.email, '')), lower($2)) > 0) \
             AND ($3::text IS NULL OR a.access = $3) AND ($4::text IS NULL OR a.template_id = $4) \
             ORDER BY i.subject LIMIT $5",
        )
        .bind(&request.after)
        .bind(&filter.q)
        .bind(filter.access.map(Access::as_str))
        .bind(&filter.template_id)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(AssignmentSummary {
                    id: row.get("id"),
                    identity: identity(row),
                    access: Access::parse(row.get("access")).ok_or(StoreError::Unavailable)?,
                    template: VersionRef {
                        id: row.get("template_id"),
                        version: row.get("template_version"),
                    },
                    version: row.get("version"),
                    instance_id: row.get("instance_id"),
                    phase: phase(row)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(page(items, request, |item: &AssignmentSummary| {
            item.identity.subject.clone()
        }))
    }

    async fn assignment(&self, id: Uuid) -> Result<Option<AssignmentView>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        let Some(record) = crate::assignments::load(&mut tx, id, false).await? else {
            return Ok(None);
        };
        let identity_row =
            sqlx::query("SELECT issuer, subject FROM manager.identities WHERE id = $1")
                .bind(record.identity_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(store_error)?;
        let instance_row = sqlx::query(
            "SELECT n.id, n.assignment_id, n.namespace, n.desired_generation, n.observed_generation, \
             n.execution_generation, n.phase, n.blocked_reason, n.conditions, \
             to_char(n.observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS observed_at \
             FROM manager.instances n WHERE n.id = $1",
        )
        .bind(record.instance_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(Some(AssignmentView {
            id: record.id,
            identity_id: record.identity_id,
            identity: identity(&identity_row),
            spec: record.spec,
            version: record.version,
            instance: instance(&instance_row)?,
        }))
    }

    async fn instance(&self, id: Uuid) -> Result<Option<InstanceView>, StoreError> {
        let row = sqlx::query(
            "SELECT n.id, n.assignment_id, n.namespace, n.desired_generation, n.observed_generation, \
             n.execution_generation, n.phase, n.blocked_reason, n.conditions, \
             to_char(n.observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS observed_at \
             FROM manager.instances n WHERE n.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        row.as_ref().map(instance).transpose()
    }

    async fn catalog(
        &self,
        kind: CatalogKind,
        q: Option<&str>,
        request: &PageRequest,
    ) -> Result<Page<CatalogSummary>, StoreError> {
        let rows = sqlx::query(
            "SELECT c.id, count(*) AS versions, \
             max(c.version) FILTER (WHERE c.state = 'published') AS latest_published, \
             max(c.version) FILTER (WHERE c.state = 'draft') AS draft, \
             CASE WHEN $1 = 'template' \
                THEN (SELECT count(*) FROM manager.assignments a WHERE a.template_id = c.id) \
                ELSE (SELECT count(*) FROM manager.assignment_backends b WHERE b.backend_id = c.id) END AS assignments \
             FROM manager.catalog_versions c WHERE c.kind = $1 \
             AND ($2::text IS NULL OR c.id > $2) AND ($3::text IS NULL OR strpos(c.id, lower($3)) > 0) \
             GROUP BY c.id ORDER BY c.id LIMIT $4",
        )
        .bind(kind.as_str())
        .bind(&request.after)
        .bind(q)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| CatalogSummary {
                id: row.get("id"),
                versions: row.get("versions"),
                latest_published: row.get("latest_published"),
                draft: row.get("draft"),
                assignments: row.get("assignments"),
            })
            .collect();
        Ok(page(items, request, |item: &CatalogSummary| {
            item.id.clone()
        }))
    }

    async fn catalog_versions(
        &self,
        kind: CatalogKind,
        id: &str,
        request: &PageRequest,
    ) -> Result<Page<VersionSummary>, StoreError> {
        let after: Option<i64> = match &request.after {
            Some(cursor) => Some(cursor.parse().map_err(|_| StoreError::Unavailable)?),
            None => None,
        };
        let rows = sqlx::query(
            "SELECT c.id, c.version, c.state, c.revision, \
             to_char(c.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at, \
             CASE WHEN $1 = 'template' \
                THEN (SELECT count(*) FROM manager.assignments a WHERE a.template_id = c.id AND a.template_version = c.version) \
                ELSE (SELECT count(*) FROM manager.assignment_backends b \
                      WHERE b.backend_id = c.id AND b.backend_version = c.version) END AS assignments \
             FROM manager.catalog_versions c WHERE c.kind = $1 AND c.id = $2 \
             AND ($3::bigint IS NULL OR c.version < $3) ORDER BY c.version DESC LIMIT $4",
        )
        .bind(kind.as_str())
        .bind(id)
        .bind(after)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(VersionSummary {
                    id: row.get("id"),
                    version: row.get("version"),
                    state: VersionState::parse(row.get("state")).ok_or(StoreError::Unavailable)?,
                    revision: row.get("revision"),
                    updated_at: row.get("updated_at"),
                    assignments: row.get("assignments"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(page(items, request, |item: &VersionSummary| {
            item.version.to_string()
        }))
    }

    async fn catalog_version<D: Definition>(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<VersionView<D>>, StoreError> {
        let row = sqlx::query(
            "SELECT c.id, c.version, c.state, c.revision, c.definition, \
             to_char(c.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at, \
             CASE WHEN $1 = 'template' \
                THEN (SELECT count(*) FROM manager.assignments a WHERE a.template_id = c.id AND a.template_version = c.version) \
                ELSE (SELECT count(*) FROM manager.assignment_backends b \
                      WHERE b.backend_id = c.id AND b.backend_version = c.version) END AS assignments \
             FROM manager.catalog_versions c WHERE c.kind = $1 AND c.id = $2 AND c.version = $3",
        )
        .bind(D::KIND.as_str())
        .bind(id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        let Some(row) = row else { return Ok(None) };
        let Json(definition) = row
            .try_get::<Json<D>, _>("definition")
            .map_err(|_| StoreError::Unavailable)?;
        Ok(Some(VersionView {
            id: row.get("id"),
            version: row.get("version"),
            state: VersionState::parse(row.get("state")).ok_or(StoreError::Unavailable)?,
            revision: row.get("revision"),
            updated_at: row.get("updated_at"),
            assignments: row.get("assignments"),
            definition,
        }))
    }

    async fn volumes(
        &self,
        source: Option<&str>,
        request: &PageRequest,
    ) -> Result<Page<VolumeUse>, StoreError> {
        let (after_assignment, after_name) = match &request.after {
            Some(cursor) => {
                let (id, name) = cursor.split_once('/').ok_or(StoreError::Unavailable)?;
                (
                    Some(Uuid::parse_str(id).map_err(|_| StoreError::Unavailable)?),
                    Some(name.to_owned()),
                )
            }
            None => (None, None),
        };
        let rows = sqlx::query(
            "SELECT v.assignment_id, i.issuer, i.subject, v.name, v.source, v.path, v.size_gib, v.read_only \
             FROM manager.assignment_volumes v JOIN manager.assignments a ON a.id = v.assignment_id \
             JOIN manager.identities i ON i.id = a.identity_id \
             WHERE ($1::text IS NULL OR v.source = $1) \
             AND ($2::uuid IS NULL OR (v.assignment_id, v.name) > ($2, $3)) \
             ORDER BY v.assignment_id, v.name LIMIT $4",
        )
        .bind(source)
        .bind(after_assignment)
        .bind(after_name)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| VolumeUse {
                assignment_id: row.get("assignment_id"),
                identity: identity(row),
                name: row.get("name"),
                source: row.get("source"),
                path: row.get("path"),
                size_gib: row.get::<Option<i32>, _>("size_gib").map(|s| s as u32),
                read_only: row.get("read_only"),
            })
            .collect();
        Ok(page(items, request, |item: &VolumeUse| {
            format!("{}/{}", item.assignment_id, item.name)
        }))
    }

    async fn secrets(&self, request: &PageRequest) -> Result<Page<SecretSummary>, StoreError> {
        let rows = sqlx::query(
            "WITH latest AS (SELECT DISTINCT ON (name) name, kind, version, created_at \
                FROM manager.admin_secrets ORDER BY name, version DESC) \
             SELECT l.name, l.kind, l.version, \
             to_char(l.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at, \
             (SELECT count(*) FROM manager.catalog_versions c WHERE \
                (c.kind = 'template' AND c.definition -> 'secrets' @> jsonb_build_array(jsonb_build_object('name', l.name))) \
                OR (c.kind = 'terminal_backend' AND c.definition ->> 'registrySecret' = l.name)) \
             + (SELECT count(*) FROM manager.assignments a \
                WHERE a.secrets @> jsonb_build_array(jsonb_build_object('name', l.name))) AS refs \
             FROM latest l WHERE ($1::text IS NULL OR l.name > $1) ORDER BY l.name LIMIT $2",
        )
        .bind(&request.after)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(SecretSummary {
                    name: row.get("name"),
                    kind: SecretKind::parse(row.get("kind")).ok_or(StoreError::Unavailable)?,
                    version: row.get("version"),
                    updated_at: row.get("updated_at"),
                    references: row.get("refs"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(page(items, request, |item: &SecretSummary| {
            item.name.clone()
        }))
    }

    async fn administrators(
        &self,
        request: &PageRequest,
    ) -> Result<Page<AdministratorView>, StoreError> {
        let rows = sqlx::query(
            "SELECT g.identity_id, i.issuer, i.subject, g.active, g.can_manage_administrators, g.version, \
             to_char(g.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at \
             FROM manager.administrator_grants g JOIN manager.identities i ON i.id = g.identity_id \
             WHERE ($1::text IS NULL OR i.subject > $1) ORDER BY i.subject LIMIT $2",
        )
        .bind(&request.after)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows.iter().map(administrator).collect();
        Ok(page(items, request, |item: &AdministratorView| {
            item.identity.subject.clone()
        }))
    }

    async fn administrator(
        &self,
        identity_id: Uuid,
    ) -> Result<Option<AdministratorView>, StoreError> {
        let row = sqlx::query(
            "SELECT g.identity_id, i.issuer, i.subject, g.active, g.can_manage_administrators, g.version, \
             to_char(g.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS updated_at \
             FROM manager.administrator_grants g JOIN manager.identities i ON i.id = g.identity_id \
             WHERE g.identity_id = $1",
        )
        .bind(identity_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(row.as_ref().map(administrator))
    }

    async fn audit(
        &self,
        filter: &AuditFilter,
        request: &PageRequest,
    ) -> Result<Page<AuditView>, StoreError> {
        let (after_time, after_id) = match &request.after {
            Some(cursor) => {
                let (time, id) = cursor.split_once('/').ok_or(StoreError::Unavailable)?;
                let time: i64 = time.parse().map_err(|_| StoreError::Unavailable)?;
                (
                    Some(time),
                    Some(Uuid::parse_str(id).map_err(|_| StoreError::Unavailable)?),
                )
            }
            None => (None, None),
        };
        let rows = sqlx::query(
            "SELECT id, actor_kind, actor_issuer, actor_subject, action, target_kind, target_id, request_id, change, \
             (extract(epoch FROM occurred_at) * 1000000)::bigint AS micros, \
             to_char(occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS occurred \
             FROM manager.audit_events \
             WHERE ($1::text IS NULL OR target_kind = $1) AND ($2::text IS NULL OR target_id = $2) \
             AND ($3::bigint IS NULL OR ((extract(epoch FROM occurred_at) * 1000000)::bigint, id) < ($3, $4)) \
             ORDER BY occurred_at DESC, id DESC LIMIT $5",
        )
        .bind(&filter.target_kind)
        .bind(&filter.target_id)
        .bind(after_time)
        .bind(after_id)
        .bind(fetch_limit(request))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let mut cursors = Vec::new();
        let mut items = Vec::new();
        for row in &rows {
            cursors.push(format!(
                "{}/{}",
                row.get::<i64, _>("micros"),
                row.get::<Uuid, _>("id")
            ));
            items.push(audit(row)?);
        }
        let more = items.len() > request.limit as usize;
        items.truncate(request.limit as usize);
        let next_cursor = more.then(|| cursors[items.len() - 1].clone());
        Ok(Page { items, next_cursor })
    }

    async fn audit_event(&self, id: Uuid) -> Result<Option<AuditView>, StoreError> {
        let row = sqlx::query(
            "SELECT id, actor_kind, actor_issuer, actor_subject, action, target_kind, target_id, request_id, change, \
             to_char(occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS occurred \
             FROM manager.audit_events WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?;
        row.as_ref().map(audit).transpose()
    }
}

fn administrator(row: &PgRow) -> AdministratorView {
    AdministratorView {
        identity_id: row.get("identity_id"),
        identity: identity(row),
        active: row.get("active"),
        can_manage_administrators: row.get("can_manage_administrators"),
        version: row.get("version"),
        updated_at: row.get("updated_at"),
    }
}

fn audit(row: &PgRow) -> Result<AuditView, StoreError> {
    let issuer: Option<String> = row.get("actor_issuer");
    let subject: Option<String> = row.get("actor_subject");
    let Json(change) = row
        .try_get::<Json<serde_json::Value>, _>("change")
        .map_err(|_| StoreError::Unavailable)?;
    Ok(AuditView {
        id: row.get("id"),
        occurred_at: row.get("occurred"),
        actor: AuditActor {
            kind: row.get("actor_kind"),
            identity: issuer
                .zip(subject)
                .map(|(issuer, subject)| Identity { issuer, subject }),
        },
        action: row.get("action"),
        target: AuditTargetView {
            kind: row.get("target_kind"),
            id: row.get("target_id"),
        },
        request_id: row.get("request_id"),
        change,
    })
}
