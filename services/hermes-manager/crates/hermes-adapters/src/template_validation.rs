//! Offline, non-mutating validation of the typed template and reviewed settings.
use hermes_application::commissioning::TemplateDefinition;
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Problem {
    pub pointer: String,
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
}
pub(crate) fn problem(pointer: String, code: &'static str) -> Problem {
    Problem {
        pointer,
        code,
        severity: "error",
        message: match code {
            "sensitive_field" => "Use approved credential references and private delivery paths.",
            "managed_execution_required" => {
                "Managed execution and its required plugin must remain enabled."
            }
            _ => "This setting does not satisfy the supported configuration contract.",
        },
    }
}
fn envelope() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .should_validate_formats(false)
            .build(
                &serde_json::from_str(include_str!(
                    "../../../../../contracts/hermes-manager/schemas/template-v1.json"
                ))
                .expect("checked-in schema"),
            )
            .expect("valid offline schema")
    })
}
fn managed() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .should_validate_formats(false)
            .build(
                &serde_json::from_str(include_str!(
                    "../../../../../contracts/hermes-manager/schemas/hermes-managed-v1.json"
                ))
                .expect("checked-in schema"),
            )
            .expect("valid offline schema")
    })
}
fn inspect(value: &Value, path: &str, depth: u8, problems: &mut Vec<Problem>) {
    if problems.len() >= 64 {
        return;
    }
    if depth > 16 {
        problems.push(problem(path.into(), "depth_limit"));
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if problems.len() >= 64 {
                    break;
                }
                let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
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
                    problems.push(problem(pointer, "sensitive_field"));
                } else {
                    inspect(value, &pointer, depth + 1, problems);
                }
            }
        }
        Value::Number(number)
            if number.as_u64().is_some_and(|v| v > 9_007_199_254_740_991)
                || number.as_i64().is_some_and(|v| v < -9_007_199_254_740_991)
                || number
                    .as_f64()
                    .is_some_and(|v| v.fract() == 0.0 && v.abs() > 9_007_199_254_740_991.0) =>
        {
            problems.push(problem(path.into(), "integer_precision"))
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                inspect(value, &format!("{path}/{index}"), depth + 1, problems);
            }
        }
        _ => {}
    }
}
pub(crate) fn validate(value: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    for error in envelope().iter_errors(value).take(64) {
        problems.push(problem(error.instance_path().to_string(), "schema"));
    }
    if let Some(config) = value.get("adminConfig") {
        for error in managed().iter_errors(config).take(64 - problems.len()) {
            problems.push(problem(
                format!("/adminConfig{}", error.instance_path()),
                "schema",
            ));
        }
        inspect(config, "/adminConfig", 1, &mut problems);
        if serde_json::to_vec(config).map_or(true, |bytes| bytes.len() > 8192) {
            problems.push(problem("/adminConfig".into(), "size_limit"));
        }
        if config
            .get("terminal")
            .and_then(|v| v.get("backend"))
            .is_some_and(|v| v != "managed_execution")
        {
            problems.push(problem(
                "/adminConfig/terminal/backend".into(),
                "managed_execution_required",
            ));
        }
        if config
            .get("plugins")
            .and_then(|v| v.get("disabled"))
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|v| v == "managed_execution"))
        {
            problems.push(problem(
                "/adminConfig/plugins/disabled".into(),
                "managed_execution_required",
            ));
        }
    }
    if problems.is_empty()
        && serde_json::from_value::<TemplateDefinition>(value.clone())
            .map_or(true, |typed| typed.validate().is_err())
    {
        problems.push(problem("".into(), "policy"));
    }
    problems.truncate(64);
    problems
}
/// JSON objects reject duplicate keys before converting to the typed envelope.
pub(crate) struct StrictValue(pub Value);
impl<'de> serde::Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("unambiguous JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| StrictValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(value.into())))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                    let StrictValue(value) = map.next_value()?;
                    object.insert(key, value);
                }
                Ok(StrictValue(Value::Object(object)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_json_rejects_duplicate_keys_at_every_depth_and_unsafe_integers() {
        for text in [
            r#"{"schemaVersion":1,"schemaVersion":2}"#,
            r#"{"adminConfig":{"a":1,"a":2}}"#,
        ] {
            assert!(serde_json::from_str::<StrictValue>(text).is_err());
        }
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../../../contracts/hermes-manager/schemas/fixtures/valid/template.json"
        ))
        .unwrap();
        value["adminConfig"]["advanced"] = serde_json::json!({"large":1e40});
        assert!(
            validate(&value)
                .iter()
                .any(|problem| problem.code == "integer_precision")
        );
    }
    #[test]
    fn shared_fixtures_and_safe_policy_diagnostics() {
        let root = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../contracts/hermes-manager/schemas/fixtures"
        ));
        for (folder, valid) in [("valid", true), ("invalid", false)] {
            for entry in std::fs::read_dir(root.join(folder)).unwrap() {
                let path = entry.unwrap().path();
                let value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                let problems = validate(&value);
                assert_eq!(problems.is_empty(), valid, "{}", path.display());
                assert!(
                    !serde_json::to_string(&problems)
                        .unwrap()
                        .contains("fixture-private-value")
                );
            }
        }
    }
}
