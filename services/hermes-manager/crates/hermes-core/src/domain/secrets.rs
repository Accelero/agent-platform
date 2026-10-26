//! Admin secrets: write-only values that templates and assignments reference
//! by name. Values never appear in definitions, responses or audit.

use serde::{Deserialize, Serialize};

use super::names::validate_name;
use super::{ErrorKind, Problems, Rejection};

pub const MAX_SECRET_VALUE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    /// Delivered as an environment variable.
    Env,
    /// Delivered as a read-only file under `/run/hermes/admin-secrets/`.
    File,
    /// Image pull credentials (`.dockerconfigjson`).
    Registry,
}

impl SecretKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Registry => "registry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "env" => Some(Self::Env),
            "file" => Some(Self::File),
            "registry" => Some(Self::Registry),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRejected {
    InvalidValue,
    InUse,
    Unknown,
    StaleVersion,
}

impl Rejection for SecretRejected {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidValue => ErrorKind::Invalid,
            Self::InUse => ErrorKind::Conflict,
            Self::Unknown => ErrorKind::NotFound,
            Self::StaleVersion => ErrorKind::Precondition,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::InvalidValue => "invalid_secret_value",
            Self::InUse => "secret_in_use",
            Self::Unknown => "not_found",
            Self::StaleVersion => "stale_version",
        }
    }
}

/// Checks a value before it is sealed. Registry values are a Docker config
/// with an `auths` object.
pub fn validate_value(kind: SecretKind, value: &[u8]) -> Result<(), SecretRejected> {
    if value.is_empty() || value.len() > MAX_SECRET_VALUE {
        return Err(SecretRejected::InvalidValue);
    }
    match kind {
        SecretKind::Env => {
            let text = std::str::from_utf8(value).map_err(|_| SecretRejected::InvalidValue)?;
            if text.contains('\0') {
                return Err(SecretRejected::InvalidValue);
            }
        }
        SecretKind::File => {}
        SecretKind::Registry => {
            let config: serde_json::Value =
                serde_json::from_slice(value).map_err(|_| SecretRejected::InvalidValue)?;
            if !config
                .get("auths")
                .is_some_and(serde_json::Value::is_object)
            {
                return Err(SecretRejected::InvalidValue);
            }
        }
    }
    Ok(())
}

/// How a template or assignment uses a secret: as an environment variable or
/// as a file, never both.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretReference {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// Environment variables the platform or the Hermes image sets itself.
const RESERVED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "TZ",
    "PYTHONPATH",
    "PYTHONDONTWRITEBYTECODE",
    "API_SERVER_ENABLED",
    "TERMINAL_ENV",
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

fn valid_env(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.starts_with("HERMES_")
        && !name.starts_with("KUBERNETES_")
        && !RESERVED_ENV.contains(&name)
}

/// Checks references: names, exactly one delivery each, no duplicate targets.
pub fn validate_references(references: &[SecretReference], at: &str, problems: &mut Problems) {
    let mut targets = Vec::new();
    for (index, reference) in references.iter().enumerate() {
        let pointer = format!("{at}/{index}");
        problems.check(
            validate_name(&reference.name).is_ok(),
            format!("{pointer}/name"),
            "invalid_name",
        );
        let target = match (&reference.env, &reference.file) {
            (Some(env), None) => {
                problems.check(valid_env(env), format!("{pointer}/env"), "invalid_name");
                format!("env:{env}")
            }
            (None, Some(file)) => {
                problems.check(
                    validate_name(file).is_ok(),
                    format!("{pointer}/file"),
                    "invalid_name",
                );
                format!("file:{file}")
            }
            _ => {
                problems.add(pointer, "one_delivery_required");
                continue;
            }
        };
        if targets.contains(&target) {
            problems.add(pointer, "duplicate_target");
        }
        targets.push(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_bounded_and_registry_values_are_docker_configs() {
        assert!(validate_value(SecretKind::Env, b"sk-test").is_ok());
        assert!(validate_value(SecretKind::Env, b"").is_err());
        assert!(validate_value(SecretKind::Env, b"a\0b").is_err());
        assert!(validate_value(SecretKind::File, &[0xff; 64]).is_ok());
        assert!(validate_value(SecretKind::File, &vec![1; MAX_SECRET_VALUE + 1]).is_err());
        assert!(
            validate_value(
                SecretKind::Registry,
                br#"{"auths":{"ghcr.io":{"auth":"eA=="}}}"#
            )
            .is_ok()
        );
        assert!(validate_value(SecretKind::Registry, br#"{"other":{}}"#).is_err());
    }

    #[test]
    fn references_name_one_delivery_and_safe_variables() {
        let reference = |env: Option<&str>, file: Option<&str>| SecretReference {
            name: "llm-key".into(),
            env: env.map(Into::into),
            file: file.map(Into::into),
        };
        let check = |references: &[SecretReference]| {
            let mut problems = Problems::default();
            validate_references(references, "/secrets", &mut problems);
            problems
                .into_vec()
                .into_iter()
                .map(|p| p.code)
                .collect::<Vec<_>>()
        };
        assert!(
            check(&[
                reference(Some("OPENAI_API_KEY"), None),
                reference(None, Some("ca"))
            ])
            .is_empty()
        );
        assert_eq!(
            check(&[reference(Some("OPENAI_API_KEY"), Some("x"))]),
            ["one_delivery_required"]
        );
        assert_eq!(check(&[reference(None, None)]), ["one_delivery_required"]);
        for env in ["HERMES_TOKEN", "PATH", "lower", "1ABC"] {
            assert_eq!(
                check(&[reference(Some(env), None)]),
                ["invalid_name"],
                "{env}"
            );
        }
        assert_eq!(
            check(&[reference(Some("KEY"), None), reference(Some("KEY"), None)]),
            ["duplicate_target"]
        );
    }
}
