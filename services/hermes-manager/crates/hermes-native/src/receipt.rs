//! Verification of the home-initialization receipt written by the image's
//! `personal-init.py`. The receipt is HMAC-SHA-256 signed with the instance's
//! initialization key and bound to the installation, instance, key row and
//! exact owner. Canonical JSON matches Python's
//! `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.

use hermes_core::app::controller::InitBinding;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::materials::hex;

const COMPLETION: &[u8] = b"hermes-home-completion-v1\0";
const MAX_RECEIPT: usize = 4096;

/// Canonical JSON: sorted object keys, no whitespace, non-ASCII kept.
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{}:{}", Value::String(key.clone()), canonical(&map[key])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        scalar => scalar.to_string(),
    }
}

/// What `personal-init.py` binds the home to.
pub fn binding_record(binding: &InitBinding) -> Value {
    let owner = json!({"issuer": binding.owner.issuer, "subject": binding.owner.subject});
    json!({
        "schemaVersion": 1,
        "installationId": binding.installation_id.to_string(),
        "instanceId": binding.instance_id.to_string(),
        "materialId": binding.material_id.to_string(),
        "ownerDigest": hex(&Sha256::digest(canonical(&owner).as_bytes())),
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Whether `receipt` is a completion receipt for `binding`, signed with `key`
/// (the initialization key as 64 hex characters).
pub fn verify(key: &[u8], binding: &InitBinding, receipt: &str) -> bool {
    let Ok(key) = std::str::from_utf8(key) else {
        return false;
    };
    let Some(key) = unhex(key.trim())
        .filter(|k| k.len() == 32)
        .map(Zeroizing::new)
    else {
        return false;
    };
    if receipt.len() > MAX_RECEIPT {
        return false;
    }
    let Ok(Value::Object(envelope)) = serde_json::from_str::<Value>(receipt) else {
        return false;
    };
    if envelope.len() != 2 {
        return false;
    }
    let (Some(record), Some(Value::String(signature))) =
        (envelope.get("record"), envelope.get("signature"))
    else {
        return false;
    };
    let Some(signature) = unhex(signature).filter(|s| s.len() == 32) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&key) else {
        return false;
    };
    mac.update(COMPLETION);
    mac.update(canonical(record).as_bytes());
    if mac.verify_slice(&signature).is_err() {
        return false;
    }
    record.get("complete") == Some(&Value::Bool(true))
        && record.pointer("/intent/binding") == Some(&binding_record(binding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::domain::identity::Identity;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    #[test]
    fn canonical_json_sorts_keys_and_keeps_unicode() {
        let value = json!({"b": [1, {"z": true, "a": null}], "a": "é/\"x\"\n"});
        assert_eq!(
            canonical(&value),
            r#"{"a":"é/\"x\"\n","b":[1,{"a":null,"z":true}]}"#
        );
    }

    fn binding() -> InitBinding {
        InitBinding {
            installation_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            material_id: Uuid::new_v4(),
            owner: Identity {
                issuer: "https://sso.example.org/realms/agents".into(),
                subject: "Owner-A".into(),
            },
        }
    }

    /// Runs the image's real initializer on a temporary home.
    fn initialize(binding: &InitBinding, key: &str) -> String {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("home");
        std::fs::create_dir(&root).unwrap();
        let context = json!({
            "schemaVersion": 1,
            "installationId": binding.installation_id.to_string(),
            "instanceId": binding.instance_id.to_string(),
            "materialId": binding.material_id.to_string(),
            "owner": {"issuer": binding.owner.issuer, "subject": binding.owner.subject},
            "template": {"id": "personal-default", "version": 1},
        });
        let files = [
            ("context.json", context.to_string()),
            (
                "config.json",
                json!({"terminal": {"backend": "managed_execution"}}).to_string(),
            ),
            ("key", key.to_owned()),
        ];
        for (name, content) in &files {
            std::fs::write(temporary.path().join(name), content).unwrap();
        }
        let program = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../runtimes/hermes/personal-init.py");
        let result = temporary.path().join("result.json");
        let mut child = Command::new("python3")
            .arg(program)
            .args(["--root", root.to_str().unwrap()])
            .args([
                "--context",
                temporary.path().join("context.json").to_str().unwrap(),
            ])
            .args([
                "--config",
                temporary.path().join("config.json").to_str().unwrap(),
            ])
            .args(["--key", temporary.path().join("key").to_str().unwrap()])
            .args(["--result", result.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "initializer failed");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("initializer deadline exceeded");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(result).unwrap()
    }

    #[test]
    fn the_real_initializer_receipt_verifies_for_exactly_its_owner_and_key() {
        let binding = binding();
        let key = "ab".repeat(32);
        let receipt = initialize(&binding, &key);
        assert!(verify(key.as_bytes(), &binding, &receipt));

        let mut other_owner = binding.clone();
        other_owner.owner.subject = "owner-a".into();
        assert!(
            !verify(key.as_bytes(), &other_owner, &receipt),
            "subjects are case-sensitive"
        );
        let mut other_material = binding.clone();
        other_material.material_id = Uuid::new_v4();
        assert!(!verify(key.as_bytes(), &other_material, &receipt));
        assert!(!verify("cd".repeat(32).as_bytes(), &binding, &receipt));
        assert!(!verify(b"not-hex", &binding, &receipt));

        let mut tampered: Value = serde_json::from_str(&receipt).unwrap();
        tampered["record"]["complete"] = json!(false);
        assert!(!verify(key.as_bytes(), &binding, &tampered.to_string()));
        assert!(!verify(key.as_bytes(), &binding, "{}"));
        assert!(!verify(
            key.as_bytes(),
            &binding,
            &"x".repeat(MAX_RECEIPT + 1)
        ));
    }
}
