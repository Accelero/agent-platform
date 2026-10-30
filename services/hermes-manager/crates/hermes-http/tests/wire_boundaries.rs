//! Wire-level boundaries of the administration API over real TLS and
//! PostgreSQL, sent as raw HTTP/1.1 bytes where a friendly client would
//! normalize the case away. Each refusal is compared with an otherwise
//! identical accepted request, so a broken fixture cannot pass vacuously, and
//! each mutation refusal leaves the audit log, the catalog and the
//! idempotency receipt for its key untouched.

#[allow(dead_code)]
mod support;

use std::time::Duration;

use hermes_testkit::contract::Contract;
use hermes_testkit::raw::{Response, chunked, head};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{Fixture, OWNER, template_fixture};

/// The administration request body limit (SPEC.md, operational bounds).
const BODY_LIMIT: usize = 16 * 1024;
/// The `adminConfig` limit, measured on its compact JSON serialization.
const ADMIN_CONFIG_LIMIT: usize = 8 * 1024;
/// Appears in refused input; no response may carry it back.
const MARKER: &str = "NEEDLE-4f7a";

struct Wire<'a> {
    fixture: &'a Fixture,
    contract: Contract,
}

impl<'a> Wire<'a> {
    async fn new(fixture: &'a Fixture) -> Self {
        let reply = fixture
            .call(
                "writeSecret",
                fixture.mutate(
                    Method::PUT,
                    "/api/v1/secrets/llm-key",
                    "seed-secret",
                    Some(&json!({"kind": "env", "value": "sk-wire-fixture"})),
                ),
            )
            .await;
        assert_eq!(reply.status, 200);
        Self {
            fixture,
            contract: Contract::load("openapi.yaml"),
        }
    }

    /// One request on its own connection: the owner's bearer, then `headers`,
    /// then `body` verbatim.
    async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &[u8])],
        body: &[u8],
    ) -> Response {
        let bearer = self.fixture.bearer(OWNER);
        let mut all: Vec<(&str, &[u8])> = vec![("Authorization", bearer.as_bytes())];
        all.extend_from_slice(headers);
        let mut raw = self.fixture.secrets.raw(&self.fixture.url).await;
        raw.send(&head(method, path, &all)).await;
        raw.send(body).await;
        raw.response().await
    }

    /// A JSON POST with a declared length.
    async fn post(&self, path: &str, key: Option<&str>, body: &[u8]) -> Response {
        let length = body.len().to_string();
        let mut headers: Vec<(&str, &[u8])> = vec![
            ("Content-Type", b"application/json"),
            ("Content-Length", length.as_bytes()),
        ];
        if let Some(key) = key {
            headers.push(("Idempotency-Key", key.as_bytes()));
        }
        self.send("POST", path, &headers, body).await
    }

    /// A JSON POST with exactly these `Idempotency-Key` header lines.
    async fn post_with_keys(&self, path: &str, body: &[u8], keys: &[&[u8]]) -> Response {
        let length = body.len().to_string();
        let mut headers: Vec<(&str, &[u8])> = vec![
            ("Content-Type", b"application/json"),
            ("Content-Length", length.as_bytes()),
        ];
        headers.extend(keys.iter().map(|key| ("Idempotency-Key", *key)));
        self.send("POST", path, &headers, body).await
    }

    /// A JSON POST without a length, in chunks of 1000 bytes.
    async fn post_chunked(&self, path: &str, key: Option<&str>, body: &[u8]) -> Response {
        let mut headers: Vec<(&str, &[u8])> = vec![
            ("Content-Type", b"application/json"),
            ("Transfer-Encoding", b"chunked"),
        ];
        if let Some(key) = key {
            headers.push(("Idempotency-Key", key.as_bytes()));
        }
        self.send("POST", path, &headers, &chunked(body, 1000))
            .await
    }

    /// The response is declared by the contract for `operation`.
    fn conforms(&self, operation: &str, response: &Response) {
        self.contract
            .check(operation, response.status, &response.json());
    }

    /// Everything a refused write must leave alone.
    async fn state(&self) -> Value {
        let audit = self
            .fixture
            .call(
                "listAuditEvents",
                self.fixture
                    .api(Method::GET, "/api/v1/audit-events?limit=100"),
            )
            .await;
        let templates = self
            .fixture
            .call(
                "listTemplates",
                self.fixture.api(Method::GET, "/api/v1/templates"),
            )
            .await;
        let assignments = self
            .fixture
            .call(
                "listAssignments",
                self.fixture.api(Method::GET, "/api/v1/assignments"),
            )
            .await;
        json!([audit.body, templates.body, assignments.body])
    }

    /// The refusal every malformed or oversized body gets, and nothing else.
    fn assert_refused(&self, operation: &str, response: &Response, case: &str) {
        assert_eq!(response.status, 422, "{case}");
        assert_eq!(
            response.json(),
            json!({"error": "invalid_request"}),
            "{case}"
        );
        self.conforms(operation, response);
        assert!(
            !String::from_utf8_lossy(&response.body).contains(MARKER),
            "{case}: input echoed"
        );
    }
}

/// `value` as compact JSON followed by spaces up to exactly `size` bytes.
fn padded(value: &Value, size: usize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= size, "fixture exceeds {size}");
    bytes.resize(size, b' ');
    bytes
}

fn draft() -> Value {
    json!({"definition": template_fixture()})
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_sized_body_is_accepted_at_exactly_16_kib_and_refused_one_byte_over(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/validate";

    for (size, accepted) in [
        (BODY_LIMIT - 1, true),
        (BODY_LIMIT, true),
        (BODY_LIMIT + 1, false),
    ] {
        let response = wire.post(path, None, &padded(&draft(), size)).await;
        if accepted {
            assert_eq!(response.status, 200, "{size} bytes");
            assert_eq!(response.json(), json!({"valid": true, "problems": []}));
            wire.conforms("validateTemplate", &response);
        } else {
            wire.assert_refused("validateTemplate", &response, "one byte over");
        }
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_streamed_body_without_a_length_has_the_same_exact_bound(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/validate";

    for (size, accepted) in [(BODY_LIMIT, true), (BODY_LIMIT + 1, false)] {
        let response = wire.post_chunked(path, None, &padded(&draft(), size)).await;
        if accepted {
            assert_eq!(response.status, 200, "{size} streamed bytes");
            assert_eq!(response.json(), json!({"valid": true, "problems": []}));
        } else {
            wire.assert_refused("validateTemplate", &response, "streamed one byte over");
        }
    }
    // Far more than the limit, in many small chunks: still one refusal, and
    // the server answers instead of buffering without bound.
    let flood = wire
        .post_chunked(path, None, &padded(&draft(), 10 * BODY_LIMIT))
        .await;
    wire.assert_refused("validateTemplate", &flood, "ten times the limit");
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn an_oversized_mutation_leaves_no_receipt_audit_or_catalog_change(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/standard/versions";
    let before = wire.state().await;

    for (name, response) in [
        (
            "sized",
            wire.post(path, Some("oversized"), &padded(&draft(), BODY_LIMIT + 1))
                .await,
        ),
        (
            "streamed",
            wire.post_chunked(path, Some("oversized"), &padded(&draft(), BODY_LIMIT + 1))
                .await,
        ),
    ] {
        wire.assert_refused("createTemplateDraft", &response, name);
        assert_eq!(wire.state().await, before, "{name}");
    }

    // The refused attempts recorded nothing for their key: the same key with
    // a different, valid body is a new change, not a conflict with a receipt.
    let mut edited = template_fixture();
    edited["resources"]["cpuMillis"] = json!(1500);
    let retry = wire
        .post(
            path,
            Some("oversized"),
            &serde_json::to_vec(&json!({"definition": edited})).unwrap(),
        )
        .await;
    assert_eq!(retry.status, 201, "{:?}", retry.json());
    wire.conforms("createTemplateDraft", &retry);
    let after = wire.state().await;
    assert_eq!(
        after[0]["items"].as_array().unwrap().len(),
        1 + before[0]["items"].as_array().unwrap().len()
    );

    // The body exactly at the limit is a valid change under the same rules.
    let exact = wire
        .post(path, Some("exact"), &padded(&draft(), BODY_LIMIT))
        .await;
    assert_eq!(exact.status, 201, "{:?}", exact.json());
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn a_request_cancelled_before_its_body_completes_commits_nothing(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/standard/versions";
    let before = wire.state().await;

    // A complete, valid document whose declared length promises more: the
    // handler must not run on the prefix that arrived.
    let mut body = serde_json::to_vec(&draft()).unwrap();
    let declared = (body.len() + 2000).to_string();
    body.truncate(body.len());
    for case in ["abandoned", "half-closed"] {
        let mut raw = fixture.secrets.raw(&fixture.url).await;
        let bearer = fixture.bearer(OWNER);
        raw.send(&head(
            "POST",
            path,
            &[
                ("Authorization", bearer.as_bytes()),
                ("Content-Type", b"application/json"),
                ("Content-Length", declared.as_bytes()),
                ("Idempotency-Key", b"cancelled"),
            ],
        ))
        .await;
        raw.send(&body).await;
        if case == "abandoned" {
            raw.abandon().await;
        } else {
            // The server may answer the short body or just close.
            raw.close_write().await;
            if let Some(response) = raw.try_response().await {
                assert_ne!(response.status / 100, 2, "{case}");
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wire.state().await, before);

    // No receipt exists for the key: a different valid body is a new change.
    let mut edited = template_fixture();
    edited["resources"]["cpuMillis"] = json!(1750);
    let retry = wire
        .post(
            path,
            Some("cancelled"),
            &serde_json::to_vec(&json!({"definition": edited})).unwrap(),
        )
        .await;
    assert_eq!(retry.status, 201, "{:?}", retry.json());
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn idempotency_keys_are_one_visible_ascii_token_of_1_to_128_bytes(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/standard/versions";
    let body = serde_json::to_vec(&draft()).unwrap();
    let before = wire.state().await;
    let refused: [(&str, Vec<&[u8]>); 7] = [
        ("empty", vec![b""]),
        ("129 bytes", vec![&[b'k'; 129]]),
        ("non-ASCII", vec!["clé".as_bytes()]),
        ("invalid UTF-8", vec![&[0xff, 0xfe]]),
        ("embedded space", vec![b"two words"]),
        ("repeated header", vec![b"first", b"second"]),
        ("absent", vec![]),
    ];
    for (case, keys) in refused {
        let response = wire.post_with_keys(path, &body, &keys).await;
        wire.assert_refused("createTemplateDraft", &response, case);
    }
    // DEL is not a header-value byte: the HTTP parser may refuse the message
    // itself before the API sees it.
    let control = wire.post_with_keys(path, &body, &[&[b'a', 0x7f]]).await;
    assert!(matches!(control.status, 400 | 422), "{}", control.status);
    assert_eq!(wire.state().await, before, "no refusal wrote anything");

    // The bounds themselves are accepted.
    for key in [vec![b'k'; 1], vec![b'k'; 128], b"!~#%+".to_vec()] {
        let response = wire.post_with_keys(path, &body, &[key.as_slice()]).await;
        assert_eq!(response.status, 201, "{} bytes", key.len());
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn if_match_is_one_quoted_positive_version(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let base = "/api/v1/templates/standard/versions";
    let created = wire
        .post(base, Some("draft"), &serde_json::to_vec(&draft()).unwrap())
        .await;
    assert_eq!(created.status, 201);
    let publish = format!("{base}/1/publish");
    let before = wire.state().await;

    let refused: [(&str, Vec<&[u8]>); 11] = [
        ("unquoted", vec![b"1"]),
        ("zero", vec![b"\"0\""]),
        ("negative", vec![b"\"-1\""]),
        ("explicit plus sign", vec![b"\"+1\""]),
        ("beyond 64 bits", vec![b"\"99999999999999999999\""]),
        ("weak", vec![b"W/\"1\""]),
        ("star", vec![b"*"]),
        ("list", vec![b"\"1\", \"2\""]),
        ("leading zero", vec![b"\"01\""]),
        ("repeated header", vec![b"\"1\"", b"\"1\""]),
        ("absent", vec![]),
    ];
    for (case, tags) in refused {
        let mut headers: Vec<(&str, &[u8])> =
            vec![("Idempotency-Key", b"publish"), ("Content-Length", b"0")];
        headers.extend(tags.into_iter().map(|tag| ("If-Match", tag)));
        let response = wire.send("POST", &publish, &headers, b"").await;
        wire.assert_refused("publishTemplate", &response, case);
    }
    assert_eq!(wire.state().await, before);

    let published = wire
        .send(
            "POST",
            &publish,
            &[
                ("Idempotency-Key", b"publish"),
                ("Content-Length", b"0"),
                ("If-Match", b"\"1\""),
            ],
            b"",
        )
        .await;
    assert_eq!(published.status, 200, "{:?}", published.json());
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn only_json_content_types_reach_a_handler(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/validate";
    let body = serde_json::to_vec(&draft()).unwrap();
    let length = body.len().to_string();

    for content_type in [
        Some(&b"application/json"[..]),
        Some(&b"application/json; charset=utf-8"[..]),
    ] {
        let mut headers: Vec<(&str, &[u8])> = vec![("Content-Length", length.as_bytes())];
        headers.extend(content_type.map(|value| ("Content-Type", value)));
        let response = wire.send("POST", path, &headers, &body).await;
        assert_eq!(response.status, 200, "{content_type:?}");
    }
    let refused: [(&str, Vec<&[u8]>); 8] = [
        ("absent", vec![]),
        ("form", vec![b"application/x-www-form-urlencoded"]),
        ("plain text", vec![b"text/plain"]),
        ("text json", vec![b"text/json"]),
        ("json suffix", vec![b"application/jsonp"]),
        ("structured suffix", vec![b"application/json+x"]),
        ("multipart", vec![b"multipart/form-data; boundary=x"]),
        ("repeated header", vec![b"application/json", b"text/plain"]),
    ];
    for (case, types) in refused {
        let mut headers: Vec<(&str, &[u8])> = vec![("Content-Length", length.as_bytes())];
        headers.extend(types.into_iter().map(|value| ("Content-Type", value)));
        let response = wire.send("POST", path, &headers, &body).await;
        wire.assert_refused("validateTemplate", &response, case);
    }
}

fn definition_with(edit: impl FnOnce(&mut String)) -> Vec<u8> {
    let mut text = serde_json::to_string(&draft()).unwrap();
    edit(&mut text);
    text.into_bytes()
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn malformed_or_ambiguous_json_is_refused_without_echo_or_effect(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let valid = serde_json::to_vec(&draft()).unwrap();
    let marked = |text: &mut String| *text = text.replace("llm-key", MARKER);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("whitespace only", b"  \n ".to_vec()),
        ("null document", b"null".to_vec()),
        ("array document", format!("[\"{MARKER}\"]").into_bytes()),
        ("string document", format!("\"{MARKER}\"").into_bytes()),
        ("number document", b"7".to_vec()),
        (
            "byte order mark",
            [b"\xEF\xBB\xBF".as_slice(), &valid].concat(),
        ),
        (
            "invalid UTF-8 in a string",
            definition_with(|text| *text = text.replace("llm-key", "llm-\u{1}key"))
                .into_iter()
                .map(|byte| if byte == 1 { 0xff } else { byte })
                .collect(),
        ),
        (
            "trailing comma",
            definition_with(|text| {
                marked(text);
                text.insert(text.len() - 1, ',');
            }),
        ),
        (
            "trailing garbage",
            definition_with(|text| {
                marked(text);
                text.push_str(" x");
            }),
        ),
        (
            "two documents",
            definition_with(|text| {
                marked(text);
                let again = text.clone();
                text.push_str(&again);
            }),
        ),
        (
            "single-quoted strings",
            definition_with(|text| {
                marked(text);
                *text = text.replace('"', "'");
            }),
        ),
        (
            "comment",
            definition_with(|text| {
                marked(text);
                text.insert_str(1, "/* x */");
            }),
        ),
        (
            "NaN",
            definition_with(|text| {
                marked(text);
                *text = text.replace("1000", "NaN");
            }),
        ),
        (
            "lone surrogate escape",
            definition_with(|text| {
                marked(text);
                *text = text.replace(MARKER, "\\ud800");
            }),
        ),
        (
            "unsafe integer",
            definition_with(|text| {
                marked(text);
                *text = text.replace("1000", "9007199254740993");
            }),
        ),
        (
            "integer at 2^53",
            definition_with(|text| {
                marked(text);
                *text = text.replace("1000", "9007199254740992");
            }),
        ),
        (
            "exponent beyond range",
            definition_with(|text| {
                marked(text);
                *text = text.replace("1000", "1e999");
            }),
        ),
        ("deeply nested arrays", {
            let depth = 6000;
            [vec![b'['; depth], vec![b']'; depth]].concat()
        }),
        ("deeply nested objects", {
            let depth = 2500;
            let mut text = r#"{"a":"#.repeat(depth);
            text.push('1');
            text.push_str(&"}".repeat(depth));
            text.into_bytes()
        }),
        (
            "duplicate top-level key",
            definition_with(|text| {
                marked(text);
                let key = r#""definition":"#;
                let start = text.find(key).unwrap();
                let value = text[start + key.len()..text.len() - 1].to_owned();
                text.insert_str(text.len() - 1, &format!(",{key}{value}"));
            }),
        ),
        (
            "duplicate nested key",
            definition_with(|text| {
                marked(text);
                *text = text.replace(
                    r#""cpuMillis":1000"#,
                    r#""cpuMillis":1000,"cpuMillis":1001"#,
                );
            }),
        ),
        (
            "duplicate key spelled with an escape",
            definition_with(|text| {
                marked(text);
                *text = text.replace(
                    r#""cpuMillis":1000"#,
                    r#""cpuMillis":1000,"cpuMillis":1001"#,
                );
            }),
        ),
        (
            "unknown field",
            definition_with(|text| {
                marked(text);
                text.insert_str(text.len() - 1, &format!(r#","{MARKER}":1"#));
            }),
        ),
    ];
    let before = wire.state().await;
    for (case, body) in &cases {
        let response = wire.post("/api/v1/templates/validate", None, body).await;
        wire.assert_refused("validateTemplate", &response, case);
        let response = wire
            .post(
                "/api/v1/templates/standard/versions",
                Some("malformed"),
                body,
            )
            .await;
        wire.assert_refused("createTemplateDraft", &response, case);
    }
    assert_eq!(
        wire.state().await,
        before,
        "no malformed body wrote anything"
    );

    // None of them recorded a receipt for the shared key.
    let accepted = wire
        .post(
            "/api/v1/templates/standard/versions",
            Some("malformed"),
            &valid,
        )
        .await;
    assert_eq!(accepted.status, 201, "{:?}", accepted.json());
}

/// A template whose `adminConfig` serializes to exactly `size` bytes.
fn template_with_admin_config(size: usize, filler: &str) -> Value {
    let mut definition = template_fixture();
    // {"x":"…"} is 8 bytes around the filler.
    let overhead = serde_json::to_vec(&json!({"x": ""})).unwrap().len();
    let units = (size - overhead) / filler.len();
    let padding = size - overhead - units * filler.len();
    let value = format!("{}{}", filler.repeat(units), "a".repeat(padding));
    definition["adminConfig"] = json!({"x": value});
    assert_eq!(
        serde_json::to_vec(&definition["adminConfig"])
            .unwrap()
            .len(),
        size
    );
    definition
}

/// The request body for [`template_with_admin_config`].
fn draft_with(size: usize, filler: &str) -> Value {
    json!({"definition": template_with_admin_config(size, filler)})
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn admin_config_is_limited_to_8_kib_of_serialized_json(pool: PgPool) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    let path = "/api/v1/templates/validate";

    for (case, filler) in [
        ("ASCII", "a"),
        ("two-byte characters", "é"),
        ("emoji", "😀"),
    ] {
        for (size, accepted) in [
            (ADMIN_CONFIG_LIMIT - 1, true),
            (ADMIN_CONFIG_LIMIT, true),
            (ADMIN_CONFIG_LIMIT + 1, false),
        ] {
            let body = serde_json::to_vec(&draft_with(size, filler)).unwrap();
            assert!(
                body.len() < BODY_LIMIT,
                "{case}: body within the request bound"
            );
            let response = wire.post(path, None, &body).await;
            assert_eq!(response.status, 200, "{case} {size}");
            wire.conforms("validateTemplate", &response);
            let reply = response.json();
            if accepted {
                assert_eq!(
                    reply,
                    json!({"valid": true, "problems": []}),
                    "{case} {size}"
                );
            } else {
                assert_eq!(
                    reply,
                    json!({"valid": false, "problems": [
                        {"pointer": "/adminConfig", "code": "too_large", "severity": "error"}
                    ]}),
                    "{case} {size}"
                );
            }
        }
    }

    // The limit is on the compact serialization, not on the wire spelling.
    let pretty = serde_json::to_vec_pretty(&draft_with(ADMIN_CONFIG_LIMIT, "a")).unwrap();
    assert!(pretty.len() > ADMIN_CONFIG_LIMIT + 200 && pretty.len() < BODY_LIMIT);
    assert_eq!(wire.post(path, None, &pretty).await.status, 200);
    for (size, accepted) in [(ADMIN_CONFIG_LIMIT, true), (ADMIN_CONFIG_LIMIT + 1, false)] {
        let mut text = serde_json::to_string(&draft_with(size, "a")).unwrap();
        let start = text.find(r#""x":""#).unwrap() + 5;
        // 1000 characters spelled as six-byte escapes on the wire.
        text.replace_range(start..start + 1000, &"\\u0061".repeat(1000));
        assert!(text.len() > ADMIN_CONFIG_LIMIT + 4000 && text.len() < BODY_LIMIT);
        let reply = wire.post(path, None, text.as_bytes()).await;
        assert_eq!(reply.status, 200, "escaped {size}");
        assert_eq!(reply.json()["valid"], accepted, "escaped {size}");
    }
}

/// Publishes `standard` v1 and the `ssh` backend v1 through the API.
async fn publish_catalog(wire: &Wire<'_>) {
    for (collection, id, definition) in [
        ("templates", "standard", template_fixture()),
        ("terminal-backends", "ssh", support::backend_fixture()),
    ] {
        let base = format!("/api/v1/{collection}/{id}/versions");
        let created = wire
            .post(
                &base,
                Some(&format!("create-{id}")),
                &serde_json::to_vec(&json!({"definition": definition})).unwrap(),
            )
            .await;
        assert_eq!(created.status, 201, "{:?}", created.json());
        let publish = wire
            .send(
                "POST",
                &format!("{base}/1/publish"),
                &[
                    ("Idempotency-Key", format!("publish-{id}").as_bytes()),
                    ("If-Match", b"\"1\""),
                    ("Content-Length", b"0"),
                ],
                b"",
            )
            .await;
        assert_eq!(publish.status, 200, "{:?}", publish.json());
    }
}

#[sqlx::test(migrator = "hermes_store::MIGRATOR")]
async fn an_assignments_admin_config_has_the_same_8_kib_bound_and_a_refusal_commits_nothing(
    pool: PgPool,
) {
    let fixture = Fixture::new(pool).await;
    let wire = Wire::new(&fixture).await;
    publish_catalog(&wire).await;

    let assignment = |subject: &str, size: usize| {
        let overhead = serde_json::to_vec(&json!({"x": ""})).unwrap().len();
        json!({
            "identity": fixture.identity_json(subject),
            "spec": {
                "access": "enabled",
                "template": {"id": "standard", "version": 1},
                "terminal": {"default": "ssh", "allowed": {"ssh": 1}},
                "adminConfig": {"x": "a".repeat(size - overhead)}
            }
        })
    };
    let before = wire.state().await;
    let over = wire
        .post(
            "/api/v1/assignments",
            Some("over"),
            &serde_json::to_vec(&assignment("bob", ADMIN_CONFIG_LIMIT + 1)).unwrap(),
        )
        .await;
    assert_eq!(over.status, 422, "{:?}", over.json());
    wire.conforms("createAssignment", &over);
    assert_eq!(
        over.json()["problems"],
        json!([{"pointer": "/spec/adminConfig", "code": "too_large", "severity": "error"}])
    );
    assert_eq!(wire.state().await, before, "the refusal changed nothing");

    let exact = wire
        .post(
            "/api/v1/assignments",
            Some("exact"),
            &serde_json::to_vec(&assignment("bob", ADMIN_CONFIG_LIMIT)).unwrap(),
        )
        .await;
    assert_eq!(exact.status, 202, "{:?}", exact.json());
    wire.conforms("createAssignment", &exact);
}
