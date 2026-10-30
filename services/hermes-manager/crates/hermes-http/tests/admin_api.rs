//! Every administration endpoint over real TLS and PostgreSQL. Responses are
//! checked against the contract by `Fixture::call`.

#[allow(dead_code)]
mod support;

use hermes_core::domain::secrets::SecretKind;
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{Fixture, Reply, backend_fixture, template_fixture};

const LLM_KEY: &str = "sk-fixture-llm-value";

impl Fixture {
    async fn write_secret(&self, name: &str, kind: &str, value: &str, key: &str) -> Reply {
        let body = json!({"kind": kind, "value": value});
        self.call(
            "writeSecret",
            self.mutate(
                Method::PUT,
                &format!("/api/v1/secrets/{name}"),
                key,
                Some(&body),
            ),
        )
        .await
    }

    /// Creates and publishes one version of a catalog entry.
    async fn publish(&self, collection: &str, id: &str, definition: &Value) -> i64 {
        let (create, publish) = match collection {
            "templates" => ("createTemplateDraft", "publishTemplate"),
            _ => ("createTerminalBackendDraft", "publishTerminalBackend"),
        };
        let base = format!("/api/v1/{collection}/{id}/versions");
        let draft = json!({"definition": definition});
        let created = self
            .call(
                create,
                self.mutate(
                    Method::POST,
                    &base,
                    &format!("create-{id}-{}", digest(definition)),
                    Some(&draft),
                ),
            )
            .await;
        assert_eq!(created.status, 201, "{:?}", created.body);
        let version = created.body["version"].as_i64().unwrap();
        let revision = created.body["revision"].as_i64().unwrap();
        let published = self
            .call(
                publish,
                self.mutate(
                    Method::POST,
                    &format!("{base}/{version}/publish"),
                    &format!("publish-{id}-{version}"),
                    None,
                )
                .header("if-match", format!("\"{revision}\"")),
            )
            .await;
        assert_eq!(published.status, 200, "{:?}", published.body);
        version
    }

    /// The LLM key, the `standard` template v1 and the `ssh` backend v1.
    async fn seed(&self) {
        assert_eq!(
            self.write_secret("llm-key", "env", LLM_KEY, "seed-secret")
                .await
                .status,
            200
        );
        assert_eq!(
            self.publish("templates", "standard", &template_fixture())
                .await,
            1
        );
        assert_eq!(
            self.publish("terminal-backends", "ssh", &backend_fixture())
                .await,
            1
        );
    }

    async fn assign(&self, subject: &str, key: &str) -> Reply {
        let request = json!({
            "identity": self.identity_json(subject),
            "spec": {
                "access": "enabled",
                "template": {"id": "standard", "version": 1},
                "terminal": {"default": "ssh", "allowed": {"ssh": 1}}
            }
        });
        self.call(
            "createAssignment",
            self.mutate(Method::POST, "/api/v1/assignments", key, Some(&request)),
        )
        .await
    }
}

fn digest(value: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn if_match(version: i64) -> String {
    format!("\"{version}\"")
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn templates_move_from_draft_to_published_to_withdrawn(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let draft = json!({"definition": template_fixture()});
    let validate = |key: &'static str| {
        fixture.call(
            "validateTemplate",
            fixture
                .api(Method::POST, "/api/v1/templates/validate")
                .header("x-case", key)
                .json(&draft),
        )
    };
    let missing = validate("missing").await;
    assert_eq!(missing.status, 200);
    assert_eq!(
        missing.body,
        json!({"valid": false, "problems": [{"pointer": "/secrets/0/name", "code": "unknown_secret", "severity": "error"}]})
    );
    fixture
        .write_secret("llm-key", "env", LLM_KEY, "secret")
        .await;
    assert_eq!(
        validate("present").await.body,
        json!({"valid": true, "problems": []})
    );

    let base = "/api/v1/templates/standard/versions";
    let created = fixture
        .call(
            "createTemplateDraft",
            fixture.mutate(Method::POST, base, "create", Some(&draft)),
        )
        .await;
    assert_eq!(
        (created.status, &created.body),
        (201, &json!({"id": "standard", "version": 1, "revision": 1}))
    );

    let read = fixture
        .call(
            "getTemplateVersion",
            fixture.api(Method::GET, &format!("{base}/1")),
        )
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(read.tag(), if_match(1));
    assert_eq!(read.body["state"], "draft");
    let mut canonical = template_fixture();
    canonical["mounts"]["workspace"]["readOnly"] = json!(false);
    canonical["defaultVolumes"]["workspace"]["readOnly"] = json!(false);
    assert_eq!(
        read.body["definition"], canonical,
        "stored in canonical form"
    );

    let mut edited = template_fixture();
    edited["resources"]["cpuMillis"] = json!(1500);
    let edit_body = json!({"definition": edited});
    let edit = |key: &'static str, revision: i64| {
        fixture.call(
            "editTemplateDraft",
            fixture
                .mutate(Method::PATCH, &format!("{base}/1"), key, Some(&edit_body))
                .header("if-match", if_match(revision)),
        )
    };
    assert_eq!(edit("edit", 1).await.body["revision"], 2);
    let stale = edit("edit-stale", 1).await;
    assert_eq!((stale.status, stale.error()), (412, "stale_version"));

    let publish = fixture
        .call(
            "publishTemplate",
            fixture
                .mutate(Method::POST, &format!("{base}/1/publish"), "publish", None)
                .header("if-match", if_match(2)),
        )
        .await;
    assert_eq!(
        (publish.status, &publish.body),
        (200, &json!({"id": "standard", "version": 1, "revision": 3}))
    );
    let frozen = edit("edit-published", 3).await;
    assert_eq!((frozen.status, frozen.error()), (409, "version_not_draft"));

    let next = fixture
        .call(
            "createTemplateDraft",
            fixture.mutate(Method::POST, base, "create-2", Some(&draft)),
        )
        .await;
    assert_eq!(next.body["version"], 2);
    let list = fixture
        .call(
            "listTemplates",
            fixture.api(Method::GET, "/api/v1/templates"),
        )
        .await;
    assert_eq!(
        list.body,
        json!({"items": [{"id": "standard", "versions": 2, "latestPublished": 1, "draft": 2, "assignments": 0}], "nextCursor": null})
    );
    let versions = fixture
        .call("listTemplateVersions", fixture.api(Method::GET, base))
        .await;
    assert_eq!(versions.body["items"].as_array().unwrap().len(), 2);

    let withdraw = fixture
        .call(
            "withdrawTemplate",
            fixture
                .mutate(
                    Method::POST,
                    &format!("{base}/1/withdraw"),
                    "withdraw",
                    None,
                )
                .header("if-match", if_match(3)),
        )
        .await;
    assert_eq!(withdraw.body["revision"], 4);
    let withdrawn = fixture
        .call(
            "getTemplateVersion",
            fixture.api(Method::GET, &format!("{base}/1")),
        )
        .await;
    assert_eq!(withdrawn.body["state"], "withdrawn");

    for path in [
        format!("{base}/9"),
        format!("{base}/0"),
        "/api/v1/templates/Bad_Name/versions/1".into(),
    ] {
        let missing = fixture
            .call("getTemplateVersion", fixture.api(Method::GET, &path))
            .await;
        assert_eq!(
            (missing.status, missing.error()),
            (404, "not_found"),
            "{path}"
        );
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn terminal_backends_share_the_catalog_lifecycle(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let base = "/api/v1/terminal-backends/ssh/versions";
    let draft = json!({"definition": backend_fixture()});
    let created = fixture
        .call(
            "createTerminalBackendDraft",
            fixture.mutate(Method::POST, base, "create", Some(&draft)),
        )
        .await;
    assert_eq!(
        created.body,
        json!({"id": "ssh", "version": 1, "revision": 1})
    );
    let mut wider = backend_fixture();
    wider["limits"]["maxEnvironments"] = json!(8);
    let edited = fixture
        .call(
            "editTerminalBackendDraft",
            fixture
                .mutate(
                    Method::PATCH,
                    &format!("{base}/1"),
                    "edit",
                    Some(&json!({"definition": wider})),
                )
                .header("if-match", if_match(1)),
        )
        .await;
    assert_eq!(edited.body["revision"], 2);
    let read = fixture
        .call(
            "getTerminalBackendVersion",
            fixture.api(Method::GET, &format!("{base}/1")),
        )
        .await;
    assert_eq!(
        (
            read.tag(),
            &read.body["definition"]["limits"]["maxEnvironments"]
        ),
        (if_match(2), &json!(8))
    );
    let published = fixture
        .call(
            "publishTerminalBackend",
            fixture
                .mutate(Method::POST, &format!("{base}/1/publish"), "publish", None)
                .header("if-match", if_match(2)),
        )
        .await;
    assert_eq!(published.body["revision"], 3);
    let withdrawn = fixture
        .call(
            "withdrawTerminalBackend",
            fixture
                .mutate(
                    Method::POST,
                    &format!("{base}/1/withdraw"),
                    "withdraw",
                    None,
                )
                .header("if-match", if_match(3)),
        )
        .await;
    assert_eq!(withdrawn.body["revision"], 4);
    let versions = fixture
        .call(
            "listTerminalBackendVersions",
            fixture.api(Method::GET, base),
        )
        .await;
    assert_eq!(versions.body["items"][0]["state"], "withdrawn");
    let list = fixture
        .call(
            "listTerminalBackends",
            fixture.api(Method::GET, "/api/v1/terminal-backends?q=ss"),
        )
        .await;
    assert_eq!(
        list.body,
        json!({"items": [{"id": "ssh", "versions": 1, "latestPublished": null, "draft": null, "assignments": 0}], "nextCursor": null}),
        "a withdrawn version is no longer the latest published"
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn publication_refuses_definitions_the_installation_cannot_honour(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let mut template = template_fixture();
    template["home"]["source"] = json!("unapproved");
    template["secrets"] = json!([]);
    let draft = json!({"definition": template});
    let created = fixture
        .call(
            "createTemplateDraft",
            fixture.mutate(
                Method::POST,
                "/api/v1/templates/odd/versions",
                "create",
                Some(&draft),
            ),
        )
        .await;
    assert_eq!(created.status, 201, "drafts may be incomplete");
    let refused = fixture
        .call(
            "publishTemplate",
            fixture
                .mutate(
                    Method::POST,
                    "/api/v1/templates/odd/versions/1/publish",
                    "publish",
                    None,
                )
                .header("if-match", if_match(1)),
        )
        .await;
    assert_eq!(
        (refused.status, refused.error()),
        (422, "invalid_configuration")
    );
    assert!(
        refused.body["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["pointer"].as_str().unwrap().starts_with("/home")
                || p["pointer"] == "/defaultVolumes/workspace/source"),
        "{:?}",
        refused.body
    );

    let mut root = backend_fixture();
    root["uid"] = json!(0);
    let backend = fixture
        .call(
            "validateTerminalBackend",
            fixture
                .api(Method::POST, "/api/v1/terminal-backends/validate")
                .json(&json!({"definition": root})),
        )
        .await;
    assert_eq!(backend.body["valid"], false);

    let mut unknown = backend_fixture();
    unknown["privileged"] = json!(true);
    let structural = fixture
        .call(
            "validateTerminalBackend",
            fixture
                .api(Method::POST, "/api/v1/terminal-backends/validate")
                .json(&json!({"definition": unknown})),
        )
        .await;
    assert_eq!(
        (structural.status, structural.error()),
        (422, "invalid_request")
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn assignments_commission_one_instance_and_changes_are_versioned(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.seed().await;

    let created = fixture.assign("bob", "assign-bob").await;
    assert_eq!(created.status, 202, "{:?}", created.body);
    assert_eq!(created.body["version"], 1);
    let assignment_id = created.body["assignmentId"].as_str().unwrap().to_owned();
    let instance_id = created.body["instanceId"].as_str().unwrap().to_owned();

    let replay = fixture.assign("bob", "assign-bob").await;
    assert_eq!(
        (replay.status, &replay.body),
        (202, &created.body),
        "same key and body replays the receipt"
    );
    let reused = fixture.assign("carol", "assign-bob").await;
    assert_eq!(
        (reused.status, reused.error()),
        (409, "idempotency_conflict")
    );
    let twice = fixture.assign("bob", "assign-bob-again").await;
    assert_eq!((twice.status, twice.error()), (409, "assignment_exists"));

    let path = format!("/api/v1/assignments/{assignment_id}");
    let read = fixture
        .call("getAssignment", fixture.api(Method::GET, &path))
        .await;
    assert_eq!(read.tag(), if_match(1));
    assert_eq!(read.body["identity"], fixture.identity_json("bob"));
    assert_eq!(
        read.body["spec"]["volumes"]["workspace"],
        json!({"source": "nvme", "sizeGib": 10, "readOnly": false}),
        "template default volumes are copied into the assignment"
    );
    assert_eq!(read.body["instance"]["id"], instance_id.as_str());
    assert_eq!(read.body["instance"]["phase"], "provisioning");
    assert!(
        read.body["instance"]["namespace"]
            .as_str()
            .unwrap()
            .starts_with("hi-")
    );

    for (query, expected) in [
        ("", 1),
        ("?access=suspended", 0),
        ("?templateId=standard", 1),
        ("?q=bob", 1),
        ("?q=nobody", 0),
    ] {
        let page = fixture
            .call(
                "listAssignments",
                fixture.api(Method::GET, &format!("/api/v1/assignments{query}")),
            )
            .await;
        assert_eq!(
            page.body["items"].as_array().unwrap().len(),
            expected,
            "{query}"
        );
    }

    let preview = |changes: Value| {
        fixture.call(
            "previewAssignment",
            fixture
                .api(Method::POST, &format!("{path}/preview"))
                .json(&changes),
        )
    };
    let config = preview(json!({"adminConfig": {"model": {"default": "fixture-model"}}})).await;
    assert_eq!(config.body["valid"], true);
    assert_eq!(config.body["restartsHermes"], true);
    let invalid = preview(json!({"volumes": {"workspace": {"source": "unapproved"}}})).await;
    assert_eq!(invalid.body["valid"], false);
    assert_eq!(
        invalid.body["problems"][0]["pointer"],
        "/volumes/workspace/source"
    );

    let update = |key: &'static str, version: i64, changes: Value| {
        fixture.call(
            "updateAssignment",
            fixture
                .mutate(Method::PATCH, &path, key, Some(&changes))
                .header("if-match", if_match(version)),
        )
    };
    let suspended = update("suspend", 1, json!({"access": "suspended"})).await;
    assert_eq!(
        (suspended.status, &suspended.body["version"]),
        (202, &json!(2))
    );
    let stale = update("suspend-stale", 1, json!({"access": "enabled"})).await;
    assert_eq!((stale.status, stale.error()), (412, "stale_version"));
    let unusable = update(
        "unusable",
        2,
        json!({"template": {"id": "standard", "version": 7}}),
    )
    .await;
    assert_eq!(
        (unusable.status, unusable.error()),
        (422, "template_unavailable")
    );

    let retried = fixture
        .call(
            "retryAssignment",
            fixture
                .mutate(Method::POST, &format!("{path}/retry"), "retry", None)
                .header("if-match", if_match(2)),
        )
        .await;
    assert_eq!(retried.status, 202);
    let instance = fixture
        .call(
            "getInstance",
            fixture.api(Method::GET, &format!("/api/v1/instances/{instance_id}")),
        )
        .await;
    assert_eq!(instance.body["assignmentId"], assignment_id.as_str());

    let identities = fixture
        .call(
            "listIdentities",
            fixture.api(Method::GET, "/api/v1/identities?q=bob"),
        )
        .await;
    let bob = &identities.body["items"][0];
    assert_eq!(bob["assignmentId"], assignment_id.as_str());
    assert_eq!(bob["verified"], false);
    let one = fixture
        .call(
            "getIdentity",
            fixture.api(
                Method::GET,
                &format!("/api/v1/identities/{}", bob["id"].as_str().unwrap()),
            ),
        )
        .await;
    assert_eq!(one.body["identity"], fixture.identity_json("bob"));

    let volumes = fixture
        .call(
            "listVolumes",
            fixture.api(Method::GET, "/api/v1/volumes?source=nvme"),
        )
        .await;
    assert_eq!(volumes.body["items"][0]["name"], "workspace");
    assert_eq!(
        volumes.body["items"][0]["assignmentId"],
        assignment_id.as_str()
    );

    let audit = fixture
        .call(
            "listAuditEvents",
            fixture.api(
                Method::GET,
                &format!("/api/v1/audit-events?targetKind=assignment&targetId={assignment_id}"),
            ),
        )
        .await;
    let actions: Vec<_> = audit.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["action"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        actions.contains(&"assignment.create".to_owned()),
        "{actions:?}"
    );
    for event in audit.body["items"].as_array().unwrap() {
        assert_eq!(
            event["change"]["volumes"]["workspace"],
            json!({"source": "nvme", "sizeGib": 10, "readOnly": false})
        );
    }
    let event_id = audit.body["items"][0]["id"].as_str().unwrap();
    let event = fixture
        .call(
            "getAuditEvent",
            fixture.api(Method::GET, &format!("/api/v1/audit-events/{event_id}")),
        )
        .await;
    assert_eq!(
        event.body["actor"]["identity"],
        fixture.identity_json(support::OWNER)
    );

    for (operation, missing) in [
        (
            "getAssignment",
            format!("/api/v1/assignments/{}", uuid::Uuid::new_v4()),
        ),
        ("getAssignment", "/api/v1/assignments/not-a-uuid".to_owned()),
        (
            "getInstance",
            format!("/api/v1/instances/{}", uuid::Uuid::new_v4()),
        ),
    ] {
        let reply = fixture
            .call(operation, fixture.api(Method::GET, &missing))
            .await;
        assert_eq!(reply.status, 404, "{missing}");
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn rollouts_report_each_assignment_on_its_own(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.seed().await;
    let mut second = template_fixture();
    second["resources"]["memoryMib"] = json!(3072);
    assert_eq!(fixture.publish("templates", "standard", &second).await, 2);
    let bob = fixture.assign("bob", "assign-bob").await.body;
    let carol = fixture.assign("carol", "assign-carol").await.body;

    let rollout = json!({
        "template": {"id": "standard", "version": 2},
        "items": [
            {"assignmentId": bob["assignmentId"], "expectedVersion": 1},
            {"assignmentId": carol["assignmentId"], "expectedVersion": 5}
        ]
    });
    let result = fixture
        .call(
            "rolloutTemplate",
            fixture.mutate(
                Method::POST,
                "/api/v1/assignment-rollouts",
                "rollout",
                Some(&rollout),
            ),
        )
        .await;
    assert_eq!(result.status, 200);
    let results = result.body["results"].as_array().unwrap();
    assert_eq!(results[0]["outcome"], "accepted");
    assert_eq!(results[0]["receipt"]["version"], 2);
    assert_eq!(results[1]["outcome"], "rejected");
    assert_eq!(results[1]["error"]["error"], "stale_version");

    let replay = fixture
        .call(
            "rolloutTemplate",
            fixture.mutate(
                Method::POST,
                "/api/v1/assignment-rollouts",
                "rollout",
                Some(&rollout),
            ),
        )
        .await;
    assert_eq!(
        replay.body["results"][0], results[0],
        "members replay under derived keys"
    );

    let empty = fixture
        .call(
            "rolloutTemplate",
            fixture.mutate(
                Method::POST,
                "/api/v1/assignment-rollouts",
                "rollout-empty",
                Some(&json!({"template": {"id": "standard", "version": 2}, "items": []})),
            ),
        )
        .await;
    assert_eq!(empty.status, 422);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn secrets_are_write_only_sealed_to_the_controller_and_kept_while_used(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let written = fixture
        .write_secret("llm-key", "env", LLM_KEY, "write")
        .await;
    assert_eq!(
        (written.status, &written.body),
        (200, &json!({"name": "llm-key", "version": 1}))
    );

    let (version, sealed): (i64, Vec<u8>) =
        sqlx::query_as("SELECT version, sealed FROM manager.admin_secrets WHERE name = 'llm-key'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(version, 1);
    assert!(
        !sealed
            .windows(LLM_KEY.len())
            .any(|w| w == LLM_KEY.as_bytes())
    );
    assert_eq!(
        &*fixture
            .opener
            .open("llm-key", SecretKind::Env, &sealed)
            .unwrap(),
        LLM_KEY.as_bytes()
    );
    assert!(
        fixture
            .opener
            .open("other-key", SecretKind::Env, &sealed)
            .is_err(),
        "bound to its name"
    );

    let listed = fixture
        .call("listSecrets", fixture.api(Method::GET, "/api/v1/secrets"))
        .await;
    assert_eq!(listed.body["items"][0]["name"], "llm-key");
    assert_eq!(listed.body["items"][0]["references"], 0);
    assert!(!listed.body.to_string().contains(LLM_KEY));

    let replaced = fixture
        .write_secret("llm-key", "env", "sk-rotated-value", "rotate")
        .await;
    assert_eq!(replaced.body["version"], 2);
    for (kind, value) in [
        ("env", "nul\u{0}byte"),
        ("registry", "{\"not\": \"docker config\"}"),
    ] {
        let invalid = fixture.write_secret("odd-key", kind, value, kind).await;
        assert_eq!(
            (invalid.status, invalid.error()),
            (422, "invalid_secret_value"),
            "{kind}"
        );
    }

    let mut template = template_fixture();
    template["secrets"] = json!([{"name": "llm-key", "env": "OPENAI_API_KEY"}]);
    fixture.publish("templates", "standard", &template).await;
    let used = fixture
        .call("listSecrets", fixture.api(Method::GET, "/api/v1/secrets"))
        .await;
    assert_eq!(used.body["items"][0]["references"], 1);
    let delete = |name: &'static str, key: &'static str, version: i64| {
        fixture.call(
            "deleteSecret",
            fixture
                .mutate(
                    Method::DELETE,
                    &format!("/api/v1/secrets/{name}"),
                    key,
                    None,
                )
                .header("if-match", if_match(version)),
        )
    };
    let in_use = delete("llm-key", "delete-used", 2).await;
    assert_eq!((in_use.status, in_use.error()), (409, "secret_in_use"));

    fixture
        .write_secret("spare", "file", "contents", "spare")
        .await;
    let stale = delete("spare", "delete-stale", 2).await;
    assert_eq!((stale.status, stale.error()), (412, "stale_version"));
    let deleted = delete("spare", "delete", 1).await;
    assert_eq!(deleted.status, 200);
    let gone = delete("spare", "delete-again", 1).await;
    assert_eq!(gone.status, 404);

    let audit = fixture
        .call(
            "listAuditEvents",
            fixture.api(Method::GET, "/api/v1/audit-events?targetKind=secret"),
        )
        .await;
    assert!(!audit.body.to_string().contains(LLM_KEY));
    assert!(!audit.body.to_string().contains("sk-rotated-value"));
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn administrator_grants_need_a_verified_identity_and_a_manager(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let grant = |subject: &str, key: &str, manage: bool| {
        let body =
            json!({"identity": fixture.identity_json(subject), "canManageAdministrators": manage});
        fixture.call(
            "grantAdministrator",
            fixture.mutate(Method::POST, "/api/v1/administrators", key, Some(&body)),
        )
    };
    let unverified = grant("dave", "grant-unverified", false).await;
    assert_eq!(
        (unverified.status, unverified.error()),
        (422, "identity_not_verified")
    );

    fixture.sign_in_once("dave").await;
    fixture.sign_in_once("erin").await;
    let candidates = fixture
        .call(
            "listAdministratorCandidates",
            fixture.api(Method::GET, "/api/v1/administrator-candidates"),
        )
        .await;
    assert_eq!(candidates.body["items"].as_array().unwrap().len(), 2);

    let granted = grant("dave", "grant-dave", false).await;
    assert_eq!(granted.status, 201);
    let duplicate = grant("dave", "grant-dave-again", false).await;
    assert_eq!(
        (duplicate.status, duplicate.error()),
        (409, "administrator_grant_exists")
    );

    let as_dave = fixture
        .call(
            "grantAdministrator",
            fixture
                .as_("dave", Method::POST, "/api/v1/administrators")
                .header("idempotency-key", "dave-grants")
                .json(&json!({"identity": fixture.identity_json("erin"), "canManageAdministrators": false})),
        )
        .await;
    assert_eq!((as_dave.status, as_dave.error()), (403, "access_denied"));
    let dave_reads = fixture
        .call(
            "listAssignments",
            fixture.as_("dave", Method::GET, "/api/v1/assignments"),
        )
        .await;
    assert_eq!(dave_reads.status, 200);

    let dave_path = format!(
        "/api/v1/administrators/{}",
        granted.body["identityId"].as_str().unwrap()
    );
    let read = fixture
        .call("getAdministrator", fixture.api(Method::GET, &dave_path))
        .await;
    assert_eq!(read.body["canManageAdministrators"], false);
    let promoted = fixture
        .call(
            "updateAdministrator",
            fixture
                .mutate(
                    Method::PATCH,
                    &dave_path,
                    "promote",
                    Some(&json!({"active": true, "canManageAdministrators": true})),
                )
                .header("if-match", read.tag()),
        )
        .await;
    assert_eq!(promoted.status, 200);

    let list = fixture
        .call(
            "listAdministrators",
            fixture.api(Method::GET, "/api/v1/administrators"),
        )
        .await;
    assert_eq!(list.body["items"].as_array().unwrap().len(), 2);
    let owner = list.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["identity"]["subject"] == support::OWNER)
        .unwrap()
        .clone();
    let demote_dave = fixture
        .call(
            "updateAdministrator",
            fixture
                .as_(
                    "dave",
                    Method::PATCH,
                    &format!(
                        "/api/v1/administrators/{}",
                        owner["identityId"].as_str().unwrap()
                    ),
                )
                .header("idempotency-key", "dave-demotes-owner")
                .header("if-match", if_match(owner["version"].as_i64().unwrap()))
                .json(&json!({"active": false, "canManageAdministrators": false})),
        )
        .await;
    assert_eq!(demote_dave.status, 200, "another manager remains");
    let last = fixture
        .call(
            "updateAdministrator",
            fixture
                .as_("dave", Method::PATCH, &dave_path)
                .header("idempotency-key", "dave-demotes-self")
                .header(
                    "if-match",
                    if_match(promoted.body["version"].as_i64().unwrap()),
                )
                .json(&json!({"active": true, "canManageAdministrators": false})),
        )
        .await;
    assert_eq!((last.status, last.error()), (409, "last_administrator"));
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn directory_lookup_is_optional_and_failures_are_not_empty(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let found = fixture
        .call(
            "searchDirectory",
            fixture.api(Method::GET, "/api/v1/directory-users?q=bo"),
        )
        .await;
    assert_eq!(found.body["enabled"], true);
    assert_eq!(
        found.body["items"][0]["identity"],
        fixture.identity_json("bob")
    );
    let down = fixture
        .call(
            "searchDirectory",
            fixture.api(Method::GET, "/api/v1/directory-users?q=unavailable"),
        )
        .await;
    assert_eq!((down.status, down.error()), (503, "dependency_unavailable"));
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn without_a_directory_lookup_reports_it_disabled(pool: PgPool) {
    let fixture = Fixture::without_directory(pool).await;
    let page = fixture
        .call(
            "searchDirectory",
            fixture.api(Method::GET, "/api/v1/directory-users?q=bob"),
        )
        .await;
    assert_eq!(
        page.body,
        json!({"enabled": false, "items": [], "nextCursor": null})
    );
    let sources = fixture
        .call(
            "listStorageSources",
            fixture.api(Method::GET, "/api/v1/storage-sources"),
        )
        .await;
    let ids: Vec<_> = sources.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].clone())
        .collect();
    assert_eq!(
        ids,
        [json!("host-data"), json!("nas"), json!("nvme")],
        "ordered by ID"
    );
    assert_eq!(
        sources.body["dataIdentity"],
        json!({"uid": 1000, "gid": 1000})
    );
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn requests_are_strict_and_refusals_never_echo_input(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    fixture.seed().await;
    let identity = fixture.identity_json("bob");
    let spec = json!({"access": "enabled", "template": {"id": "standard", "version": 1}});
    let cases = [
        (
            "unknown field",
            json!({"identity": identity, "spec": spec, "owner": "x"}).to_string(),
            "application/json",
            Some("k1"),
        ),
        (
            "duplicate key",
            format!(r#"{{"identity":{identity},"spec":{spec},"spec":{spec}}}"#),
            "application/json",
            Some("k2"),
        ),
        (
            "not JSON",
            "identity=bob".to_owned(),
            "application/x-www-form-urlencoded",
            Some("k3"),
        ),
        (
            "no key",
            json!({"identity": identity, "spec": spec}).to_string(),
            "application/json",
            None,
        ),
        (
            "bad key",
            json!({"identity": identity, "spec": spec}).to_string(),
            "application/json",
            Some("spaces are not allowed"),
        ),
    ];
    for (case, body, content_type, key) in cases {
        let mut request = fixture
            .api(Method::POST, "/api/v1/assignments")
            .header("content-type", content_type)
            .body(body.clone());
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let reply = fixture.call("createAssignment", request).await;
        assert_eq!(
            (reply.status, reply.error()),
            (422, "invalid_request"),
            "{case}"
        );
        assert!(!reply.body.to_string().contains("bob"), "{case}");
    }
    for query in [
        "?unknown=1",
        "?limit=0",
        "?limit=101",
        "?after=x&after=y",
        "?access=deleted",
    ] {
        let reply = fixture
            .call(
                "listAssignments",
                fixture.api(Method::GET, &format!("/api/v1/assignments{query}")),
            )
            .await;
        assert_eq!(reply.status, 422, "{query}");
    }
    let foreign = json!({"identity": {"issuer": "https://elsewhere.example.org/realms/x", "subject": "bob"}, "spec": spec});
    let reply = fixture
        .call(
            "createAssignment",
            fixture.mutate(
                Method::POST,
                "/api/v1/assignments",
                "foreign",
                Some(&foreign),
            ),
        )
        .await;
    assert_eq!((reply.status, reply.error()), (422, "foreign_issuer"));

    let assignment = fixture.assign("bob", "assign").await;
    let path = format!(
        "/api/v1/assignments/{}",
        assignment.body["assignmentId"].as_str().unwrap()
    );
    for tag in ["1", "\"0\"", "W/\"1\"", "\"one\""] {
        let reply = fixture
            .call(
                "updateAssignment",
                fixture
                    .mutate(
                        Method::PATCH,
                        &path,
                        "bad-tag",
                        Some(&json!({"access": "suspended"})),
                    )
                    .header("if-match", tag),
            )
            .await;
        assert_eq!(reply.status, 422, "{tag}");
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn people_without_a_grant_reach_no_administration_endpoint(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let reads = [
        ("listIdentities", "/api/v1/identities"),
        ("searchDirectory", "/api/v1/directory-users"),
        ("listAssignments", "/api/v1/assignments"),
        ("listTemplates", "/api/v1/templates"),
        ("listTerminalBackends", "/api/v1/terminal-backends"),
        ("listStorageSources", "/api/v1/storage-sources"),
        ("listVolumes", "/api/v1/volumes"),
        ("listSecrets", "/api/v1/secrets"),
        ("listAdministrators", "/api/v1/administrators"),
        (
            "listAdministratorCandidates",
            "/api/v1/administrator-candidates",
        ),
        ("listAuditEvents", "/api/v1/audit-events"),
        ("listOrphans", "/api/v1/orphans"),
    ];
    for (operation, path) in reads {
        let denied = fixture
            .call(operation, fixture.as_("mallory", Method::GET, path))
            .await;
        assert_eq!(
            (denied.status, denied.error()),
            (403, "access_denied"),
            "{path}"
        );
        let anonymous = fixture
            .call(
                operation,
                fixture.client.get(format!("{}{path}", fixture.url)),
            )
            .await;
        assert_eq!(anonymous.status, 401, "{path}");
    }
    let write = fixture
        .call(
            "writeSecret",
            fixture
                .as_("mallory", Method::PUT, "/api/v1/secrets/llm-key")
                .header("idempotency-key", "mallory")
                .json(&json!({"kind": "env", "value": LLM_KEY})),
        )
        .await;
    assert_eq!(write.status, 403);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM manager.admin_secrets")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn orphans_are_listed_as_the_controller_last_recorded_them(pool: PgPool) {
    use hermes_core::app::controller::InstanceStore;
    use hermes_core::domain::orphans::{Orphan, OrphanKind};

    let fixture = Fixture::new(pool).await;
    fixture.seed().await;
    let before = fixture
        .call("listOrphans", fixture.api(Method::GET, "/api/v1/orphans"))
        .await;
    assert_eq!(
        (before.status, &before.body),
        (200, &json!({"scannedAt": null, "items": []}))
    );
    let epoch = fixture.store.advance_epoch().await.unwrap();
    let stray = |kind, name: &str| Orphan {
        kind,
        name: name.into(),
        uid: format!("uid-{name}"),
    };
    assert!(
        fixture
            .store
            .record_orphans(
                epoch,
                &[
                    stray(OrphanKind::PersistentVolume, "hermes-stray-workspace"),
                    stray(OrphanKind::Namespace, "hermes-stray"),
                ],
            )
            .await
            .unwrap()
    );
    let listed = fixture
        .call("listOrphans", fixture.api(Method::GET, "/api/v1/orphans"))
        .await;
    assert_eq!(listed.status, 200);
    assert!(listed.body["scannedAt"].is_string());
    let items: Vec<_> = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["kind"].as_str().unwrap(),
                item["name"].as_str().unwrap(),
                item["uid"].as_str().unwrap(),
                item["firstSeenAt"] == item["lastSeenAt"],
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            ("namespace", "hermes-stray", "uid-hermes-stray", true),
            (
                "persistent_volume",
                "hermes-stray-workspace",
                "uid-hermes-stray-workspace",
                true
            ),
        ]
    );
    // A later scan keeps when an object was first seen.
    assert!(
        fixture
            .store
            .record_orphans(epoch, &[stray(OrphanKind::Namespace, "hermes-stray")])
            .await
            .unwrap()
    );
    let later = fixture
        .call("listOrphans", fixture.api(Method::GET, "/api/v1/orphans"))
        .await;
    let item = &later.body["items"][0];
    assert_eq!(later.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(item["firstSeenAt"], listed.body["items"][0]["firstSeenAt"]);
    assert_ne!(item["lastSeenAt"], listed.body["items"][0]["lastSeenAt"]);
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn instance_environments_are_listed_without_keys_or_addresses(pool: PgPool) {
    use hermes_core::app::controller::{InstanceStore, NewMaterial};
    use hermes_core::app::execution::{AcquireRequest, Execution, credential_digest};
    use hermes_core::domain::assignments::LifetimeMode;
    use hermes_core::domain::materials::MaterialPurpose;

    let fixture = Fixture::new(pool).await;
    fixture.seed().await;
    let assigned = fixture.assign("bob", "assign").await;
    let instance = uuid::Uuid::parse_str(assigned.body["instanceId"].as_str().unwrap()).unwrap();
    let path = format!("/api/v1/instances/{instance}/environments");
    let empty = fixture
        .call("listEnvironments", fixture.api(Method::GET, &path))
        .await;
    assert_eq!(empty.body, json!({"items": []}));

    let token = "ab".repeat(32);
    fixture
        .store
        .insert_material(
            instance,
            MaterialPurpose::ExecutionCredential,
            &NewMaterial {
                sealed: b"sealed".to_vec(),
                public: credential_digest(&token),
            },
        )
        .await
        .unwrap();
    // Two terminal changes advance the execution generation to 3.
    let assignment = format!(
        "/api/v1/assignments/{}",
        assigned.body["assignmentId"].as_str().unwrap()
    );
    for (version, key, mode) in [
        (1, "mode-on-demand", "per_profile_on_demand"),
        (2, "mode-per-session", "per_session"),
    ] {
        let terminal = json!({"terminal": {"default": "ssh", "allowed": {"ssh": 1}, "mode": mode}});
        let changed = fixture
            .call(
                "updateAssignment",
                fixture
                    .mutate(Method::PATCH, &assignment, key, Some(&terminal))
                    .header("if-match", if_match(version)),
            )
            .await;
        assert_eq!(changed.status, 202, "{mode}");
    }
    let pending = Execution::new(fixture.store.clone())
        .acquire(
            &token,
            &AcquireRequest {
                session: "s1".into(),
                mode: LifetimeMode::PerSession,
                request: "r1".into(),
                runtime_id: None,
                parent_lease: None,
                runtime: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(pending.code, "environment_pending");
    let listed = fixture
        .call("listEnvironments", fixture.api(Method::GET, &path))
        .await;
    let item = &listed.body["items"][0];
    assert_eq!(item["backend"], json!({"id": "ssh", "version": 1}));
    assert_eq!(
        (item["state"].as_str(), item["session"].as_str()),
        (Some("pending"), Some("s1"))
    );
    assert_eq!(item["failure"], json!(null));
    assert_eq!(item["generation"], json!(3));
    // A failed worker is reported by its reason, still without addresses.
    let environment = uuid::Uuid::parse_str(item["id"].as_str().unwrap()).unwrap();
    assert!(
        fixture
            .store
            .fail_environment(
                environment,
                hermes_core::domain::execution::EnvironmentState::Pending,
                hermes_core::domain::execution::WorkerFailure::ImageUnavailable,
            )
            .await
            .unwrap()
    );
    let failed = fixture
        .call("listEnvironments", fixture.api(Method::GET, &path))
        .await;
    let item = &failed.body["items"][0];
    assert_eq!(
        (item["state"].as_str(), item["failure"].as_str()),
        (Some("draining"), Some("image_unavailable"))
    );
    let missing = fixture
        .call(
            "listEnvironments",
            fixture.api(
                Method::GET,
                &format!("/api/v1/instances/{}/environments", uuid::Uuid::new_v4()),
            ),
        )
        .await;
    assert_eq!(missing.status, 404);
}
