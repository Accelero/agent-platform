//! The managed Hermes configuration: the platform-enforced layer over the
//! template's admin settings and the assignment's overrides. Hermes applies
//! it over the owner's own `config.yaml`, which it never rewrites.

use hermes_core::app::controller::DesiredInstance;
use hermes_core::domain::settings::MANAGED_BACKEND;
use serde_json::{Map, Value, json};

use crate::RenderConfig;

/// Where the Hermes container finds its credentials and trust.
pub const CREDENTIALS_DIR: &str = "/run/hermes/credentials";
pub const TRUST_DIR: &str = "/run/hermes/execution-ca";
pub const SSH_KEY: &str = "/run/hermes/credentials/id_ed25519";
pub const EXECUTION_TOKEN: &str = "/run/hermes/credentials/execution-client-token";
pub const EXECUTION_CA: &str = "/run/hermes/execution-ca/ca.crt";

fn object<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let entry = map.entry(key.to_owned()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry.as_object_mut().expect("object")
}

/// Layers 1–3: platform-enforced settings over the merged admin settings.
pub fn managed(config: &RenderConfig, desired: &DesiredInstance) -> Value {
    let mut managed = desired.admin_config.clone();
    let terminal = object(&mut managed, "terminal");
    terminal.insert("backend".into(), json!(MANAGED_BACKEND));
    terminal.insert("ssh_key".into(), json!(SSH_KEY));
    let plugins = object(&mut managed, "plugins");
    let enabled = plugins.entry("enabled").or_insert_with(|| json!([]));
    if !enabled.is_array() {
        *enabled = json!([]);
    }
    let enabled = enabled.as_array_mut().expect("array");
    if !enabled.iter().any(|name| name == MANAGED_BACKEND) {
        enabled.push(json!(MANAGED_BACKEND));
    }
    let disabled = plugins.entry("disabled").or_insert_with(|| json!([]));
    if let Some(names) = disabled.as_array_mut() {
        names.retain(|name| name != MANAGED_BACKEND);
    } else {
        *disabled = json!([]);
    }
    let entries = object(plugins, "entries");
    entries.insert(
        MANAGED_BACKEND.into(),
        json!({"settings": {
            "manager_url": config.execution_url,
            "token_file": EXECUTION_TOKEN,
            "ca_file": EXECUTION_CA,
            "ssh_key": SSH_KEY,
            "mode": desired.terminal.mode.as_str(),
            "runtime_id": desired.terminal.default.clone().unwrap_or_default(),
            "acquire_timeout_seconds": 180,
        }}),
    );
    Value::Object(managed)
}

/// The fixed seed of a fresh home's `config.yaml`. It never depends on the
/// assignment: the initializer refuses changed inputs before completion.
pub fn seed() -> Value {
    json!({
        "terminal": {"backend": MANAGED_BACKEND},
        "plugins": {"enabled": [MANAGED_BACKEND]},
    })
}
