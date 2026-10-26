//! Admin settings (`adminConfig`): Hermes configuration enforced over the
//! user's own. Platform-enforced keys cannot be set by templates or
//! assignments, and settings never carry secret values.

use serde_json::{Map, Value};

use super::Problems;

/// Serialized size limit of one `adminConfig` document.
pub const MAX_ADMIN_CONFIG: usize = 8 * 1024;

/// The managed terminal provider the platform always enforces.
pub const MANAGED_BACKEND: &str = "managed_execution";

/// Checks one admin settings layer. `at` points at the layer.
pub fn validate_admin_config(config: &Map<String, Value>, at: &str, problems: &mut Problems) {
    let size = serde_json::to_vec(config)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    problems.check(size <= MAX_ADMIN_CONFIG, at, "too_large");
    if let Some(terminal) = config.get("terminal") {
        match terminal.as_object() {
            None => problems.add(format!("{at}/terminal"), "invalid_type"),
            Some(terminal) => {
                let backend = terminal.get("backend");
                problems.check(
                    backend.is_none_or(|b| b == MANAGED_BACKEND),
                    format!("{at}/terminal/backend"),
                    "platform_enforced",
                );
                for key in ["manager_url", "token_file", "ca_file", "ssh_key"] {
                    problems.check(
                        !terminal.contains_key(key),
                        format!("{at}/terminal/{key}"),
                        "platform_enforced",
                    );
                }
            }
        }
    }
    if let Some(plugins) = config.get("plugins") {
        match plugins.as_object() {
            None => problems.add(format!("{at}/plugins"), "invalid_type"),
            Some(plugins) => {
                if let Some(disabled) = plugins.get("disabled") {
                    let ok = disabled.as_array().is_some_and(|names| {
                        names
                            .iter()
                            .all(|name| name.is_string() && name != MANAGED_BACKEND)
                    });
                    problems.check(ok, format!("{at}/plugins/disabled"), "platform_enforced");
                }
                let managed = plugins
                    .get("entries")
                    .and_then(Value::as_object)
                    .is_some_and(|entries| entries.contains_key(MANAGED_BACKEND));
                problems.check(
                    !managed,
                    format!("{at}/plugins/entries/{MANAGED_BACKEND}"),
                    "platform_enforced",
                );
            }
        }
    }
}

/// Merges `overlay` into `base` leaf by leaf: objects merge, anything else
/// replaces. Used to layer assignment overrides over template settings.
pub fn merge(base: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(update)) => merge(existing, update),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn codes(config: Value) -> Vec<(String, &'static str)> {
        let mut problems = Problems::default();
        validate_admin_config(config.as_object().unwrap(), "/adminConfig", &mut problems);
        problems
            .into_vec()
            .into_iter()
            .map(|p| (p.pointer, p.code))
            .collect()
    }

    #[test]
    fn platform_enforced_keys_cannot_be_overridden() {
        assert!(
            codes(
                json!({"model": {"provider": "custom"}, "terminal": {"runtime_id": "python-tools"}})
            )
            .is_empty()
        );
        assert_eq!(
            codes(json!({"terminal": {"backend": "local"}})),
            [(
                "/adminConfig/terminal/backend".to_owned(),
                "platform_enforced"
            )]
        );
        assert_eq!(
            codes(json!({"plugins": {"disabled": ["managed_execution"]}})),
            [(
                "/adminConfig/plugins/disabled".to_owned(),
                "platform_enforced"
            )]
        );
        assert_eq!(
            codes(json!({"plugins": {"entries": {"managed_execution": {"settings": {}}}}})),
            [(
                "/adminConfig/plugins/entries/managed_execution".to_owned(),
                "platform_enforced"
            )]
        );
        assert_eq!(
            codes(json!({"terminal": "local"})),
            [("/adminConfig/terminal".to_owned(), "invalid_type")]
        );
        assert_eq!(
            codes(json!({"x": "a".repeat(9000)})),
            [("/adminConfig".to_owned(), "too_large")]
        );
    }

    #[test]
    fn overrides_merge_leaf_by_leaf() {
        let mut base =
            json!({"model": {"provider": "custom", "default": "a"}, "agent": {"max_turns": 30}});
        let overlay = json!({"model": {"default": "b"}, "agent": 5});
        merge(base.as_object_mut().unwrap(), overlay.as_object().unwrap());
        assert_eq!(
            base,
            json!({"model": {"provider": "custom", "default": "b"}, "agent": 5})
        );
    }
}
