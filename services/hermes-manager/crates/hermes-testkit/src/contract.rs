//! Checks HTTP responses against the rewrite's OpenAPI contracts.

use serde_json::{Value, json};

const BASE: &str = "file:///contracts/";

fn read(name: &str) -> Value {
    let path = format!(
        "{}/../../../../contracts/hermes-manager/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("contract {path}"));
    serde_yaml_ng::from_str(&text).expect("contract YAML")
}

struct Contracts;

impl jsonschema::Retrieve for Contracts {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Ok(read(uri.path().as_str().trim_start_matches("/contracts/")))
    }
}

/// One contract file, for checking responses by operation ID.
pub struct Contract {
    name: String,
    document: Value,
}

impl Contract {
    pub fn load(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            document: read(name),
        }
    }

    fn operation(&self, operation_id: &str) -> &Value {
        for item in self.document["paths"].as_object().expect("paths").values() {
            for operation in item.as_object().expect("path item").values() {
                if operation["operationId"] == operation_id {
                    return operation;
                }
            }
        }
        panic!("no operation {operation_id}")
    }

    /// Panics unless `status` is declared for the operation and `body`
    /// matches its JSON schema.
    pub fn check(&self, operation_id: &str, status: u16, body: &Value) {
        let mut response = &self.operation(operation_id)["responses"][status.to_string()];
        assert!(
            !response.is_null(),
            "{operation_id} does not declare {status}"
        );
        if let Some(reference) = response.get("$ref").and_then(Value::as_str) {
            let name = reference.rsplit('/').next().expect("reference");
            response = &self.document["components"]["responses"][name];
        }
        let Some(schema) = response.pointer("/content/application~1json/schema") else {
            assert!(body.is_null(), "{operation_id} {status} declares no body");
            return;
        };
        let root = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": format!("{BASE}{}", self.name),
            "allOf": [schema],
            "components": self.document["components"],
        });
        let validator = jsonschema::options()
            .with_retriever(Contracts)
            .build(&root)
            .expect("schema compiles");
        let errors: Vec<_> = validator
            .iter_errors(body)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{operation_id} {status}: {errors:?}\n{body:#}"
        );
    }
}
