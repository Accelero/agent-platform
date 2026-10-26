//! Every response DTO serializes to a document its contract schema accepts.

use hermes_core::app::access::{ADMINISTER_ACTIONS, MANAGE_ACTION};
use hermes_core::domain::identity::Identity;
use hermes_wire::ErrorBody;
use hermes_wire::admin::Session;
use serde_json::{Value, json};

fn contract(name: &str) -> Value {
    let path = format!(
        "{}/../../../../contracts/hermes-manager/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("contract {path}"));
    serde_yaml_ng::from_str(&text).expect("contract YAML")
}

const BASE: &str = "file:///contracts/";

/// A validator for one component schema of `name`. References into other
/// contract files resolve against the same base URI.
fn schema(name: &str, component: &str) -> jsonschema::Validator {
    let document = contract(name);
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("{BASE}{name}"),
        "$ref": format!("#/components/schemas/{component}"),
        "components": document["components"],
    });
    jsonschema::options()
        .should_validate_formats(true)
        .with_retriever(Contracts)
        .build(&schema)
        .expect("schema compiles")
}

/// Serves `file:///contracts/<name>` from the repository's contract files.
struct Contracts;

impl jsonschema::Retrieve for Contracts {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let name = uri.path().as_str().trim_start_matches("/contracts/");
        Ok(contract(name))
    }
}

fn accepts(validator: &jsonschema::Validator, value: &impl serde::Serialize) -> bool {
    validator.is_valid(&serde_json::to_value(value).expect("serializes"))
}

#[test]
fn session_matches_the_administration_contract() {
    let validator = schema("openapi.yaml", "Session");
    let actor = Identity {
        issuer: "https://sso.example.org/realms/agents".into(),
        subject: "owner-a".into(),
    };
    let mut actions = vec![MANAGE_ACTION];
    actions.extend_from_slice(ADMINISTER_ACTIONS);
    assert_eq!(actions, ["administrator.manage", "administer"]);
    let browser = Session {
        actor: actor.clone(),
        permitted_actions: actions,
        csrf_token: Some("a".repeat(64)),
    };
    let bearer = Session {
        actor: actor.clone(),
        permitted_actions: ADMINISTER_ACTIONS.to_vec(),
        csrf_token: None,
    };
    assert!(accepts(&validator, &browser));
    assert!(accepts(&validator, &bearer));
    let unknown_action = Session {
        actor,
        permitted_actions: vec!["cluster.admin"],
        csrf_token: None,
    };
    assert!(
        !accepts(&validator, &unknown_action),
        "the validator must reject unknown actions"
    );
}

#[test]
fn error_bodies_use_published_codes() {
    let validator = schema("openapi.yaml", "Error");
    for code in [
        "access_denied",
        "invalid_identity_evidence",
        "dependency_unavailable",
        "registry_unavailable",
        "invalid_request",
        "not_found",
    ] {
        assert!(
            accepts(
                &validator,
                &ErrorBody {
                    error: code,
                    problems: vec![]
                }
            ),
            "{code}"
        );
    }
    assert!(!accepts(
        &validator,
        &ErrorBody {
            error: "fault_injected",
            problems: vec![]
        }
    ));
}

fn fixture(path: &str) -> Value {
    let path = format!(
        "{}/../../../../contracts/hermes-manager/schemas/fixtures/{path}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("fixture JSON")
}

fn fixtures(folder: &str, prefix: &str) -> Vec<(String, Value)> {
    let directory = format!(
        "{}/../../../../contracts/hermes-manager/schemas/fixtures/{folder}",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut found: Vec<_> = std::fs::read_dir(directory)
        .expect("fixtures")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("name")
        })
        .filter(|name| name.starts_with(prefix))
        .map(|name| {
            let value = fixture(&format!("{folder}/{name}"));
            (name, value)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

fn definition_schema(file: &str) -> jsonschema::Validator {
    let schema = contract(&format!("schemas/{file}"));
    jsonschema::options()
        .build(&schema)
        .expect("schema compiles")
}

/// The JSON Schema (used by the dashboard) and the Rust validation (used by
/// the manager) agree on every shared fixture.
#[test]
fn template_schema_and_rust_validation_agree() {
    use hermes_core::app::testing::settings;
    use hermes_core::domain::templates::TemplateDefinition;
    let schema = definition_schema("template-v2.json");
    let rust = |value: &Value| {
        serde_json::from_value::<TemplateDefinition>(value.clone())
            .is_ok_and(|definition| definition.validate(&settings().sources).is_ok())
    };
    let valid = fixture("valid/template.json");
    assert!(schema.is_valid(&valid));
    assert!(rust(&valid));
    for (name, value) in fixtures("invalid", "template-") {
        assert!(!schema.is_valid(&value), "schema accepts {name}");
        assert!(!rust(&value), "Rust accepts {name}");
    }
}

#[test]
fn terminal_backend_schema_and_rust_validation_agree() {
    use hermes_core::domain::backends::BackendDefinition;
    use hermes_core::domain::storage::DataIdentity;
    let schema = definition_schema("terminal-backend-v1.json");
    let rust = |value: &Value| {
        serde_json::from_value::<BackendDefinition>(value.clone())
            .is_ok_and(|definition| definition.validate(DataIdentity::default()).is_ok())
    };
    let valid = fixture("valid/terminal-backend.json");
    assert!(schema.is_valid(&valid));
    assert!(rust(&valid));
    for (name, value) in fixtures("invalid", "terminal-backend-") {
        assert!(!schema.is_valid(&value), "schema accepts {name}");
        assert!(!rust(&value), "Rust accepts {name}");
    }
}

#[test]
fn route_decisions_match_the_router_contract() {
    use hermes_core::app::testing::installation;
    use hermes_core::domain::routing::{
        Decision, Destination, PendingReason, Route, UnavailableReason,
    };
    use hermes_wire::routing::{DecisionBody, ResolveRequest};

    let installation = installation();
    let instance_id = uuid::Uuid::new_v4();
    let route = Route {
        instance_id,
        route_generation: 3,
        authorization_version: 2,
        destination: Destination {
            namespace: installation.instance_namespace(instance_id),
            service: "hermes",
            port: 8080,
        },
    };
    let cases = [
        ("ReadyDecision", Decision::Ready(route.clone())),
        (
            "PendingDecision",
            Decision::Pending(PendingReason::InstanceNotReady),
        ),
        (
            "PendingDecision",
            Decision::Pending(PendingReason::InstanceBlocked),
        ),
        ("DeniedDecision", Decision::Denied),
        (
            "UnavailableDecision",
            Decision::Unavailable(UnavailableReason::DependencyUnavailable),
        ),
        (
            "UnavailableDecision",
            Decision::Unavailable(UnavailableReason::RegistryUnavailable),
        ),
    ];
    for (component, decision) in &cases {
        let body = DecisionBody::from(decision);
        assert!(
            accepts(&schema("router.openapi.yaml", component), &body),
            "{component}: {body:?}"
        );
        let parsed: DecisionBody =
            hermes_wire::strict::from_slice(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(parsed, body);
    }
    assert_eq!(
        DecisionBody::from(&Decision::Ready(route.clone())).route_for(&installation),
        Some(route)
    );

    let request = schema("router.openapi.yaml", "ResolveRequest");
    let example =
        json!({"identity": {"issuer": "https://sso.example.org/realms/agents", "subject": "bob"}});
    assert!(request.is_valid(&example));
    let parsed: ResolveRequest = hermes_wire::strict::from_slice(example.to_string().as_bytes())
        .expect("the contract example parses");
    assert!(accepts(&request, &parsed));
    for refused in [
        json!({"identity": {"issuer": "https://sso.example.org/realms/agents", "subject": "bob"}, "extra": 1}),
        json!({"identity": {"issuer": "https://sso.example.org/realms/agents", "subject": "bob", "email": "b@x"}}),
    ] {
        assert!(!request.is_valid(&refused));
        assert!(
            hermes_wire::strict::from_slice::<ResolveRequest>(refused.to_string().as_bytes())
                .is_err()
        );
    }
}

#[test]
fn the_gateway_accepts_only_the_instance_service_in_its_own_namespace() {
    use hermes_core::app::testing::installation;
    use hermes_wire::routing::{DecisionBody, DestinationBody, Scheme};

    let installation = installation();
    let instance_id = uuid::Uuid::new_v4();
    let body = |namespace: String, service: &str, port: u16, scheme: Scheme| DecisionBody::Ready {
        instance_id,
        route_generation: 1,
        authorization_version: 1,
        destination: DestinationBody {
            namespace,
            service: service.into(),
            port,
            scheme,
        },
    };
    let own = installation.instance_namespace(instance_id);
    assert!(
        body(own.clone(), "hermes", 8080, Scheme::Http)
            .route_for(&installation)
            .is_some()
    );
    let other_instance = installation.instance_namespace(uuid::Uuid::new_v4());
    for refused in [
        body(other_instance, "hermes", 8080, Scheme::Http),
        body("kube-system".into(), "hermes", 8080, Scheme::Http),
        body(own.clone(), "kubernetes", 8080, Scheme::Http),
        body(own.clone(), "hermes", 9119, Scheme::Http),
        body(own, "hermes", 8080, Scheme::Https),
    ] {
        assert_eq!(refused.route_for(&installation), None, "{refused:?}");
    }
    for other in [DecisionBody::denied(), DecisionBody::unavailable()] {
        assert_eq!(other.route_for(&installation), None);
    }
}
