//! Bounded administrator list queries with query-bound keyset cursors.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardQuery {
    pub after: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u16,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub sort: String,
    pub access: Option<String>,
    pub state: Option<String>,
    pub template_id: Option<String>,
    pub action: Option<String>,
    pub actor: Option<String>,
    pub instance_id: Option<Uuid>,
    pub source: Option<String>,
    pub identity_id: Option<Uuid>,
    pub assigned: Option<bool>,
    pub runtime_id: Option<String>,
    pub runtime_version: Option<i64>,
    pub assignment_id: Option<Uuid>,
    pub publication: Option<String>,
}
fn default_limit() -> u16 {
    50
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    digest: String,
    id: String,
}
#[derive(Clone, Copy, Debug)]
pub enum DashboardList {
    CatalogVersions,
    RuntimeVersions,
    TemplateDefinitions,
    Drafts,
    Operations,
    StorageClasses,
    Bindings,
    AdministratorCandidates,
    Administrators,
    Assignments,
    Identities,
    Audit,
    Runtimes,
}
impl DashboardQuery {
    fn prepare(
        &self,
        scope: &InstallationScope,
        list: DashboardList,
    ) -> Result<(String, Option<String>), Error> {
        if !(1..=100).contains(&self.limit)
            || self.q.len() > 128
            || !matches!(self.sort.as_str(), "" | "id" | "-id")
            || self.after.as_ref().is_some_and(|value| value.len() > 2048)
        {
            return Err(Error::Invalid);
        }
        for value in [
            &self.access,
            &self.state,
            &self.template_id,
            &self.action,
            &self.actor,
            &self.source,
            &self.runtime_id,
            &self.publication,
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty() || value.len() > 255 {
                return Err(Error::Invalid);
            }
        }
        if self
            .access
            .as_ref()
            .is_some_and(|value| !matches!(value.as_str(), "enabled" | "suspended" | "revoked"))
            || (!matches!(list, DashboardList::Operations)
                && self.state.as_ref().is_some_and(|value| {
                    !matches!(
                        value.as_str(),
                        "Pending"
                            | "Provisioning"
                            | "Ready"
                            | "Blocked"
                            | "Suspending"
                            | "Suspended"
                            | "Retained"
                            | "Deleting"
                            | "Deleted"
                            | "Error"
                    )
                }))
        {
            return Err(Error::Invalid);
        }
        if self
            .source
            .as_ref()
            .is_some_and(|v| !matches!(v.as_str(), "manager" | "infrastructure"))
            || self
                .publication
                .as_ref()
                .is_some_and(|v| !matches!(v.as_str(), "draft" | "published"))
            || self.runtime_version.is_some_and(|v| v < 1)
        {
            return Err(Error::Invalid);
        }
        if matches!(list, DashboardList::Operations)
            && self.state.as_ref().is_some_and(|v| {
                !matches!(
                    v.as_str(),
                    "Pending"
                        | "Running"
                        | "Waiting"
                        | "RetryAt"
                        | "Succeeded"
                        | "Blocked"
                        | "Failed"
                        | "Superseded"
                )
            })
        {
            return Err(Error::Invalid);
        }
        if self.instance_id.is_some() != matches!(list, DashboardList::Bindings) {
            return Err(Error::Invalid);
        }
        if matches!(list, DashboardList::RuntimeVersions) && self.runtime_id.is_none() {
            return Err(Error::Invalid);
        }
        let assignment_filters =
            matches!(list, DashboardList::Assignments | DashboardList::Identities);
        if (!assignment_filters
            && (self.access.is_some()
                || (self.template_id.is_some() && !matches!(list, DashboardList::CatalogVersions))
                || self.identity_id.is_some()
                || (self.runtime_id.is_some() && !matches!(list, DashboardList::RuntimeVersions))
                || self.runtime_version.is_some()))
            || (self.assigned.is_some() && !matches!(list, DashboardList::Identities))
            || (self.source.is_some()
                && !assignment_filters
                && !matches!(list, DashboardList::TemplateDefinitions))
            || (self.publication.is_some() && !matches!(list, DashboardList::Drafts))
            || (self.state.is_some()
                && !assignment_filters
                && !matches!(list, DashboardList::Operations))
            || ((self.action.is_some() || self.actor.is_some())
                && !matches!(list, DashboardList::Audit))
            || (self.assignment_id.is_some()
                && !matches!(list, DashboardList::Operations | DashboardList::Audit))
        {
            return Err(Error::Invalid);
        }
        let mut canonical = self.clone();
        canonical.after = None;
        if canonical.sort.is_empty() {
            canonical.sort = "id".into();
        }
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(
            serde_json::to_vec(&(scope.id, scope.epoch, format!("{list:?}"), canonical))
                .map_err(|_| Error::Invalid)?,
        ));
        let after = if let Some(after) = &self.after {
            let cursor: Cursor =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(after).map_err(|_| Error::Invalid)?)
                    .map_err(|_| Error::Invalid)?;
            if cursor.digest != digest {
                return Err(Error::Invalid);
            }
            if matches!(list, DashboardList::RuntimeVersions) {
                if cursor.id.parse::<i64>().map_or(true, |v| v < 1) {
                    return Err(Error::Invalid);
                }
            } else if matches!(list, DashboardList::CatalogVersions) {
                let (id, version) = cursor.id.split_once(':').ok_or(Error::Invalid)?;
                hermes_domain::validate_name(id)?;
                if version.parse::<i64>().map_or(true, |v| v < 1) {
                    return Err(Error::Invalid);
                }
            } else if matches!(
                list,
                DashboardList::Runtimes
                    | DashboardList::TemplateDefinitions
                    | DashboardList::StorageClasses
            ) {
                hermes_domain::validate_name(&cursor.id)?;
            } else {
                Uuid::parse_str(&cursor.id).map_err(|_| Error::Invalid)?;
            }
            Some(cursor.id)
        } else {
            None
        };
        Ok((digest, after))
    }
}
impl PgStore {
    pub async fn dashboard_list(
        &self,
        scope: &InstallationScope,
        list: DashboardList,
        query: &DashboardQuery,
    ) -> Result<Value, Error> {
        let (digest, after) = query.prepare(scope, list)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let comparison = if query.sort == "-id" { "<" } else { ">" };
        let direction = if query.sort == "-id" { "DESC" } else { "ASC" };
        // Every interpolation is a fixed, reviewed SQL fragment. Submitted values are bound.
        let (select, from, id, conditions) = match list {
            DashboardList::CatalogVersions => (
                "c.id,c.version,c.image,c.native_port,c.native_scheme,c.id||':'||lpad(c.version::text,20,'0') AS cursor_id",
                "manager.instance_templates c",
                "(c.id||':'||lpad(c.version::text,20,'0'))",
                "c.approved AND c.schema_version=1 AND ($7::text IS NULL OR c.id=$7) AND (strpos(lower(c.id||' · '||c.version::text),lower($4))>0 OR strpos(lower(c.image),lower($4))>0)",
            ),
            DashboardList::RuntimeVersions => (
                "r.version AS id,r.definition,r.content_digest",
                "manager.terminal_runtime_versions r",
                "r.version",
                "r.id=$13 AND (strpos(lower(r.version::text),lower($4))>0 OR strpos(lower(r.definition->>'image'),lower($4))>0)",
            ),
            DashboardList::TemplateDefinitions => (
                "t.id,t.infrastructure,t.condition,t.rejected,t.definition,(SELECT count(*) FROM manager.assignments a WHERE a.installation_id=$1 AND a.template_id=t.id) AS assigned_count",
                "(WITH ids AS (SELECT id FROM manager.infrastructure_templates WHERE installation_id=$1 UNION SELECT id FROM manager.instance_templates WHERE approved) SELECT ids.id,i.id IS NOT NULL AS infrastructure,i.condition,i.rejected_definition IS NOT NULL AS rejected,coalesce(i.definition,d.definition) AS definition FROM ids LEFT JOIN manager.infrastructure_templates i ON i.installation_id=$1 AND i.id=ids.id LEFT JOIN LATERAL(SELECT definition FROM manager.instance_templates c JOIN manager.template_definitions d ON d.template_id=c.id AND d.template_version=c.version WHERE c.id=ids.id AND c.approved ORDER BY c.version DESC LIMIT 1) d ON true) t",
                "t.id",
                "($10::text IS NULL OR (CASE WHEN t.infrastructure THEN 'infrastructure' ELSE 'manager' END)=$10) AND (strpos(lower(t.id),lower($4))>0 OR strpos(lower(t.definition->>'image'),lower($4))>0)",
            ),
            DashboardList::Drafts => (
                "d.id,d.template_id,d.definition,d.version,d.published_version",
                "manager.dashboard_template_drafts d",
                "d.id",
                "d.installation_id=$1 AND ($16::text IS NULL OR CASE WHEN d.published_version IS NULL THEN 'draft' ELSE 'published' END=$16) AND (strpos(lower(d.template_id),lower($4))>0 OR strpos(lower(d.id::text),lower($4))>0)",
            ),
            DashboardList::Operations => (
                "o.id,o.instance_id,o.kind,o.generation,o.state,o.attempt,extract(epoch from o.next_attempt_at)::bigint AS next_attempt_at",
                "manager.operations o JOIN manager.instances i ON i.id=o.instance_id",
                "o.id",
                "o.installation_id=$1 AND ($15::uuid IS NULL OR i.assignment_id=$15) AND ($6::text IS NULL OR o.state=$6) AND (strpos(lower(o.kind),lower($4))>0 OR strpos(lower(o.id::text),lower($4))>0)",
            ),
            DashboardList::Bindings => (
                "b.id,b.definition",
                "(SELECT DISTINCT ON (b.id) b.id,b.definition FROM manager.storage_binding_versions b JOIN manager.instances i ON i.id=b.instance_id JOIN manager.assignments a ON a.id=i.assignment_id WHERE a.installation_id=$1 AND b.instance_id=$17 AND b.namespace=i.namespace AND b.namespace_uid=i.namespace_uid ORDER BY b.id,b.version DESC) b",
                "b.id",
                "(strpos(lower(b.id::text),lower($4))>0 OR strpos(lower(b.definition->>'pvcName'),lower($4))>0)",
            ),
            DashboardList::StorageClasses => (
                "c.id",
                "(SELECT DISTINCT d.definition->>'storageClass' AS id FROM manager.template_definitions d JOIN manager.instance_templates t ON t.id=d.template_id AND t.version=d.template_version WHERE t.approved) c",
                "c.id",
                "strpos(lower(c.id),lower($4))>0",
            ),
            DashboardList::AdministratorCandidates => (
                "g.id,g.issuer,g.subject",
                "manager_admin_auth.verified_identities g",
                "g.id",
                "g.installation_id=$1 AND (strpos(lower(g.subject),lower($4))>0 OR strpos(lower(g.issuer),lower($4))>0)",
            ),
            DashboardList::Administrators => (
                "g.id,g.issuer,g.subject,g.active,g.can_manage_administrators,g.version",
                "manager_admin_auth.administrator_grants g",
                "g.id",
                "g.installation_id=$1 AND (strpos(lower(g.subject),lower($4))>0 OR strpos(lower(g.issuer),lower($4))>0)",
            ),
            DashboardList::Assignments => (
                "a.id,a.infrastructure_managed,k.issuer,k.subject,a.access,a.template_id,a.template_version,a.version,i.id AS instance_id,i.namespace,i.desired_generation,i.observed_generation,i.state",
                "manager.assignments a JOIN manager.identities k ON k.id=a.identity_id JOIN manager.instances i ON i.assignment_id=a.id",
                "a.id",
                "a.installation_id=$1 AND ($5::text IS NULL OR a.access=$5) AND ($6::text IS NULL OR i.state=$6) AND ($7::text IS NULL OR a.template_id=$7) AND (strpos(lower(k.subject),lower($4))>0 OR strpos(lower(k.issuer),lower($4))>0) AND ($10::text IS NULL OR (CASE WHEN a.id IS NULL THEN NULL WHEN a.infrastructure_managed THEN 'infrastructure' ELSE 'manager' END)=$10) AND ($11::uuid IS NULL OR k.id=$11 OR EXISTS(SELECT 1 FROM manager_admin_auth.verified_identities v WHERE v.id=$11 AND v.installation_id=$1 AND v.issuer=k.issuer AND v.subject=k.subject)) AND ($13::text IS NULL OR EXISTS(SELECT 1 FROM manager.execution_policy_status ps JOIN manager.execution_policy_versions pv ON pv.instance_id=ps.instance_id AND pv.generation=ps.desired_generation CROSS JOIN LATERAL jsonb_array_elements(pv.policy->'runtimes') r WHERE ps.instance_id=i.id AND r->'definition'->'runtime'->>'id'=$13 AND ($14::bigint IS NULL OR (r->'definition'->'runtime'->>'version')::bigint=$14)))",
            ),
            DashboardList::Identities => (
                "k.id,k.issuer,k.subject,k.provenance,a.id AS assignment_id,a.access,a.template_id,a.template_version,a.infrastructure_managed,i.id AS instance_id,i.namespace,i.desired_generation,i.observed_generation,i.state,a.version",
                "(SELECT k.id,k.issuer,k.subject,'manager_record'::text AS provenance FROM manager.identities k UNION ALL SELECT v.id,v.issuer,v.subject,'verified_authentication'::text AS provenance FROM manager_admin_auth.verified_identities v WHERE v.installation_id=$1 AND NOT EXISTS(SELECT 1 FROM manager.identities k WHERE k.issuer=v.issuer AND k.subject=v.subject)) k LEFT JOIN manager.assignments a ON a.identity_id=k.id AND a.installation_id=$1 LEFT JOIN manager.instances i ON i.assignment_id=a.id",
                "k.id",
                "k.issuer=(SELECT issuer FROM manager.installations WHERE id=$1) AND ($5::text IS NULL OR a.access=$5) AND ($6::text IS NULL OR i.state=$6) AND ($7::text IS NULL OR a.template_id=$7) AND ($12::bool IS NULL OR (a.id IS NOT NULL)=$12) AND (strpos(lower(k.subject),lower($4))>0 OR strpos(lower(k.issuer),lower($4))>0) AND ($10::text IS NULL OR (CASE WHEN a.id IS NULL THEN NULL WHEN a.infrastructure_managed THEN 'infrastructure' ELSE 'manager' END)=$10) AND ($11::uuid IS NULL OR k.id=$11 OR EXISTS(SELECT 1 FROM manager_admin_auth.verified_identities v WHERE v.id=$11 AND v.installation_id=$1 AND v.issuer=k.issuer AND v.subject=k.subject)) AND ($13::text IS NULL OR EXISTS(SELECT 1 FROM manager.execution_policy_status ps JOIN manager.execution_policy_versions pv ON pv.instance_id=ps.instance_id AND pv.generation=ps.desired_generation CROSS JOIN LATERAL jsonb_array_elements(pv.policy->'runtimes') r WHERE ps.instance_id=i.id AND r->'definition'->'runtime'->>'id'=$13 AND ($14::bigint IS NULL OR (r->'definition'->'runtime'->>'version')::bigint=$14)))",
            ),
            DashboardList::Audit => (
                "CASE WHEN e.execution_event THEN 'Recorded' ELSE coalesce(o.state,d.state,'Succeeded') END AS outcome,e.id,e.actor_issuer,e.actor_subject,e.action,e.assignment_id,coalesce(e.operation_id,e.entity_operation_id) AS operation_id,e.entity_type,e.entity_id,e.request_id,e.version,extract(epoch from e.occurred_at)::bigint AS occurred_at",
                "manager.dashboard_activity e LEFT JOIN manager.operations o ON o.id=e.operation_id LEFT JOIN manager.dashboard_operations d ON d.id=e.entity_operation_id",
                "e.id",
                "e.installation_id=$1 AND ($15::uuid IS NULL OR e.assignment_id=$15) AND ($8::text IS NULL OR e.action=$8) AND ($9::text IS NULL OR e.actor_subject=$9) AND (strpos(lower(e.action),lower($4))>0 OR strpos(lower(e.actor_subject),lower($4))>0)",
            ),
            DashboardList::Runtimes => (
                "r.id,r.definition,r.content_digest",
                "(SELECT DISTINCT ON(id) id,definition,content_digest FROM manager.terminal_runtime_versions ORDER BY id,version DESC) r",
                "r.id",
                "(strpos(lower(r.id),lower($4))>0 OR strpos(lower(r.definition->>'image'),lower($4))>0)",
            ),
        };
        let cast = if matches!(list, DashboardList::RuntimeVersions) {
            "bigint"
        } else if matches!(
            list,
            DashboardList::CatalogVersions
                | DashboardList::Runtimes
                | DashboardList::TemplateDefinitions
                | DashboardList::StorageClasses
        ) {
            "text"
        } else {
            "uuid"
        };
        let mut statement = sqlx::QueryBuilder::<Postgres>::new("WITH params AS (SELECT ");
        statement
            .push_bind(scope.id)
            .push("::uuid,")
            .push_bind(after)
            .push("::text,")
            .push_bind(i64::from(query.limit) + 1)
            .push("::bigint,")
            .push_bind(&query.q)
            .push("::text,")
            .push_bind(&query.access)
            .push("::text,")
            .push_bind(&query.state)
            .push("::text,")
            .push_bind(&query.template_id)
            .push("::text,")
            .push_bind(&query.action)
            .push("::text,")
            .push_bind(&query.actor)
            .push("::text,")
            .push_bind(&query.source)
            .push("::text,")
            .push_bind(query.identity_id)
            .push("::uuid,")
            .push_bind(query.assigned)
            .push("::boolean,")
            .push_bind(&query.runtime_id)
            .push("::text,")
            .push_bind(query.runtime_version)
            .push("::bigint,")
            .push_bind(query.assignment_id)
            .push("::uuid,")
            .push_bind(&query.publication)
            .push("::text,")
            .push_bind(query.instance_id)
            .push("::uuid) SELECT ")
            .push(select)
            .push(" FROM ")
            .push(from)
            .push(" WHERE ")
            .push(conditions)
            .push(" AND ($2::text IS NULL OR ")
            .push(id)
            .push(comparison)
            .push("($2::text)::")
            .push(cast)
            .push(") ORDER BY ")
            .push(id)
            .push(" ")
            .push(direction)
            .push(" LIMIT $3");
        let rows = statement
            .build()
            .fetch_all(&mut *tx)
            .await
            .map_err(db_error)?;
        let more = rows.len() > usize::from(query.limit);
        let mut items = Vec::new();
        let mut last = None;
        for row in rows.into_iter().take(usize::from(query.limit)) {
            last = Some(if matches!(list, DashboardList::RuntimeVersions) {
                row.get::<i64, _>("id").to_string()
            } else if matches!(list, DashboardList::CatalogVersions) {
                row.get::<String, _>("cursor_id")
            } else if matches!(
                list,
                DashboardList::Runtimes
                    | DashboardList::TemplateDefinitions
                    | DashboardList::StorageClasses
            ) {
                row.get::<String, _>("id")
            } else {
                row.get::<Uuid, _>("id").to_string()
            });
            let value = match list {
                DashboardList::CatalogVersions => {
                    json!({"id":row.get::<String,_>("id"),"version":row.get::<i64,_>("version"),"image":row.get::<String,_>("image"),"nativePort":row.get::<i32,_>("native_port"),"nativeScheme":row.get::<String,_>("native_scheme")})
                }
                DashboardList::Bindings => {
                    let value: Value = row.get("definition");
                    let typed: hermes_application::execution::StorageBinding =
                        serde_json::from_value(value.clone())
                            .map_err(|_| Error::RecoveryRequired)?;
                    typed.validate()?;
                    value
                }
                DashboardList::TemplateDefinitions => template_view(&mut tx, scope, row).await?,
                DashboardList::Drafts => serde_json::to_value(super::authoring::draft_view(row)?)
                    .map_err(|_| Error::RecoveryRequired)?,
                DashboardList::StorageClasses => {
                    json!({"id":row.get::<String,_>("id"),"source":"approved_template"})
                }
                DashboardList::Operations => {
                    json!({"id":row.get::<Uuid,_>("id"),"instanceId":row.get::<Uuid,_>("instance_id"),"kind":row.get::<String,_>("kind"),"generation":row.get::<i64,_>("generation"),"state":row.get::<String,_>("state"),"attempt":row.get::<i32,_>("attempt"),"nextAttemptAt":row.get::<i64,_>("next_attempt_at")})
                }
                DashboardList::AdministratorCandidates => {
                    json!({"issuer":row.get::<String,_>("issuer"),"subject":row.get::<String,_>("subject")})
                }
                DashboardList::Administrators => super::administrators::grant_view(row),
                DashboardList::Assignments => serde_json::to_value(assignment_view(row)?)
                    .map_err(|_| Error::RecoveryRequired)?,
                DashboardList::Identities => {
                    let assignment = row.get::<Option<Uuid>,_>("assignment_id").map(|id| json!({"id":id,"instanceId":row.get::<Uuid,_>("instance_id"),"identity":{"issuer":row.get::<String,_>("issuer"),"subject":row.get::<String,_>("subject")},"source":if row.get::<bool,_>("infrastructure_managed"){"infrastructure"}else{"manager"},"access":row.get::<String,_>("access"),"template":{"id":row.get::<String,_>("template_id"),"version":row.get::<i64,_>("template_version")},"version":row.get::<i64,_>("version"),"namespace":row.get::<String,_>("namespace"),"desiredGeneration":row.get::<i64,_>("desired_generation"),"observedGeneration":row.get::<i64,_>("observed_generation"),"state":row.get::<String,_>("state")}));
                    json!({"id":row.get::<Uuid,_>("id"),"identity":{"issuer":row.get::<String,_>("issuer"),"subject":row.get::<String,_>("subject")},"directoryVerified":false,"provenance":row.get::<String,_>("provenance"),"assignment":assignment})
                }
                DashboardList::Audit => {
                    json!({"outcome":row.get::<String,_>("outcome"),"id":row.get::<Uuid,_>("id"),"actor":{"issuer":row.get::<String,_>("actor_issuer"),"subject":row.get::<String,_>("actor_subject")},"action":row.get::<String,_>("action"),"assignmentId":row.get::<Option<Uuid>,_>("assignment_id"),"entityType":row.get::<Option<String>,_>("entity_type"),"entityId":row.get::<Option<String>,_>("entity_id"),"operationId":row.get::<Option<Uuid>,_>("operation_id"),"requestId":row.get::<Uuid,_>("request_id"),"version":row.get::<i64,_>("version"),"occurredAt":row.get::<i64,_>("occurred_at")})
                }
                DashboardList::Runtimes | DashboardList::RuntimeVersions => {
                    let value: Value = row.get("definition");
                    let definition: hermes_application::execution::RuntimeDefinition =
                        serde_json::from_value(value.clone())
                            .map_err(|_| Error::RecoveryRequired)?;
                    if definition.digest()? != row.get::<String, _>("content_digest") {
                        return Err(Error::OwnershipConflict);
                    }
                    value
                }
            };
            items.push(value);
        }
        let next = if more {
            Some(
                URL_SAFE_NO_PAD.encode(
                    serde_json::to_vec(&Cursor {
                        digest,
                        id: last.ok_or(Error::RecoveryRequired)?,
                    })
                    .map_err(|_| Error::RecoveryRequired)?,
                ),
            )
        } else {
            None
        };
        tx.commit().await.map_err(db_error)?;
        Ok(json!({"items":items,"nextCursor":next}))
    }
}

/// Personal seed fields are never returned, including in rejected definitions.
pub(super) fn redact_configuration(value: &mut Value, path: &str, redacted: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                if [
                    "apikey",
                    "password",
                    "token",
                    "secret",
                    "prompt",
                    "identity",
                    "memory",
                    "profile",
                    "credential",
                    "authorization",
                ]
                .iter()
                .any(|term| normalized.contains(term))
                {
                    *value = Value::String("[redacted]".into());
                    redacted.push(pointer);
                } else {
                    redact_configuration(value, &pointer, redacted);
                }
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter_mut().enumerate() {
                redact_configuration(value, &format!("{path}/{index}"), redacted);
            }
        }
        _ => {}
    }
}
impl PgStore {
    pub async fn dashboard_templates(
        &self,
        scope: &InstallationScope,
        id: Option<&str>,
    ) -> Result<Value, Error> {
        if let Some(id) = id {
            hermes_domain::validate_name(id)?;
        }
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        gate(&mut tx, scope).await?;
        let rows=sqlx::query("WITH ids AS (SELECT id FROM manager.infrastructure_templates WHERE installation_id=$1 UNION SELECT id FROM manager.instance_templates WHERE approved) SELECT ids.id,t.id IS NOT NULL AS infrastructure,t.condition,t.rejected_definition IS NOT NULL AS rejected,coalesce(t.definition,d.definition) AS definition,(SELECT count(*) FROM manager.assignments a WHERE a.installation_id=$1 AND a.template_id=ids.id) AS assigned_count FROM ids LEFT JOIN manager.infrastructure_templates t ON t.installation_id=$1 AND t.id=ids.id LEFT JOIN LATERAL(SELECT definition FROM manager.instance_templates c JOIN manager.template_definitions d ON d.template_id=c.id AND d.template_version=c.version WHERE c.id=ids.id AND c.approved ORDER BY c.version DESC LIMIT 1) d ON true WHERE ($2::text IS NULL OR ids.id=$2) ORDER BY ids.id LIMIT 1000")
            .bind(scope.id).bind(id).fetch_all(&mut *tx).await.map_err(db_error)?;
        if id.is_some() && rows.is_empty() {
            return Err(Error::NotFound);
        }
        let mut items = Vec::new();
        for row in rows {
            items.push(template_view(&mut tx, scope, row).await?);
        }
        tx.commit().await.map_err(db_error)?;
        if id.is_some() {
            items.pop().ok_or(Error::NotFound)
        } else {
            Ok(json!({"items":items}))
        }
    }
}

async fn template_view(
    tx: &mut Transaction<'_, Postgres>,
    _scope: &InstallationScope,
    row: sqlx::postgres::PgRow,
) -> Result<Value, Error> {
    let template_id: String = row.get("id");
    let raw: Option<Value> = row.get("definition");
    let had_definition = raw.is_some();
    let parsed = raw.and_then(|value| {
        serde_json::from_value::<hermes_application::commissioning::TemplateDefinition>(value).ok()
    });
    let invalid = (had_definition && parsed.is_none())
        || parsed
            .as_ref()
            .is_some_and(|typed| typed.validate().is_err())
        || row.get::<bool, _>("rejected");
    let definition_digest = parsed
        .as_ref()
        .filter(|v| v.validate().is_ok())
        .map(|v| v.digest())
        .transpose()?;
    let mut definition = parsed
        .filter(|typed| typed.validate().is_ok())
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| Error::RecoveryRequired)?;
    let mut redacted = Vec::new();
    if let Some(config) = definition
        .as_mut()
        .and_then(|value| value.get_mut("adminConfig"))
    {
        redact_configuration(config, "/adminConfig", &mut redacted);
    }
    let versions=sqlx::query("SELECT version,content_digest FROM manager.instance_templates WHERE id=$1 AND approved ORDER BY version DESC LIMIT 50").bind(&template_id).fetch_all(&mut **tx).await.map_err(db_error)?.into_iter().map(|row|json!({"version":row.get::<i64,_>("version"),"digest":row.get::<String,_>("content_digest")})).collect::<Vec<_>>();
    let publication = if invalid {
        "rejected"
    } else if definition.is_none() {
        "catalog_only"
    } else if versions.first().and_then(|v| v["digest"].as_str()) == definition_digest.as_deref() {
        "published"
    } else {
        "pending"
    };
    let coverage = if definition
        .as_ref()
        .and_then(|v| v["image"].as_str())
        .is_some_and(|v| {
            v.ends_with("@sha256:d4da4a40cd7a28aba983775d9fd31d94cbf153eeb0cb9e844d6d0f612b7c24db")
        }) {
        "reviewed_keys"
    } else {
        "envelope_only"
    };
    Ok(
        json!({"publicationState":publication,"schemaCoverage":coverage,"id":template_id,"source":if row.get::<bool,_>("infrastructure"){"infrastructure"}else{"manager"},"definition":definition,"condition":if invalid{Some("invalid_definition".to_owned())}else{row.get::<Option<String>,_>("condition")},"rejectedDefinition":if invalid{Some(json!({"code":"invalid_definition"}))}else{None},"versions":versions,"assignedUserCount":row.get::<i64,_>("assigned_count"),"redactedPaths":redacted}),
    )
}
