//! Platform ownership and lifecycle invariants. No transport or infrastructure SDKs.
pub mod resources;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Invalid {
    #[error("invalid identity")]
    Identity,
    #[error("issuer is not configured")]
    Issuer,
    #[error("invalid version")]
    Version,
    #[error("invalid identifier")]
    Identifier,
    #[error("invalid digest")]
    Digest,
    #[error("invalid mount")]
    Mount,
}

/// Exact OIDC keys. Display names and email deliberately do not participate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
}

impl Identity {
    pub fn validate(&self, configured_issuer: &str) -> Result<(), Invalid> {
        validate_issuer(&self.issuer)?;
        if self.issuer != configured_issuer {
            return Err(Invalid::Issuer);
        }
        if self.subject.is_empty()
            || self.subject.len() > 255
            || !self.subject.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Invalid::Identity);
        }
        Ok(())
    }
}

pub fn validate_issuer(issuer: &str) -> Result<(), Invalid> {
    let url = Url::parse(issuer).map_err(|_| Invalid::Identity)?;
    if issuer.len() > 2048
        || issuer.trim() != issuer
        || issuer.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Invalid::Identity);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Enabled,
    Suspended,
    Revoked,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Suspended => "suspended",
            Self::Revoked => "revoked",
        }
    }

    pub fn desired_replicas(self) -> u8 {
        u8::from(self == Self::Enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateVersion {
    pub id: String,
    pub version: i64,
}

impl TemplateVersion {
    pub fn validate(&self) -> Result<(), Invalid> {
        validate_name(&self.id)?;
        if self.version < 1 {
            return Err(Invalid::Version);
        }
        Ok(())
    }
}

pub fn validate_name(name: &str) -> Result<(), Invalid> {
    if name.is_empty()
        || name.len() > 63
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !name.ends_with(|c: char| c.is_ascii_alphanumeric())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Invalid::Identifier);
    }
    Ok(())
}

pub fn validate_sha256(digest: &str) -> Result<(), Invalid> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Invalid::Digest);
    }
    Ok(())
}

/// 60 characters, stable across retries, and contains no personal identity.
pub fn instance_namespace(installation: Uuid, instance: Uuid) -> String {
    format!(
        "hi-{}-{}",
        &installation.simple().to_string()[..24],
        instance.simple()
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    pub storage_binding_id: Uuid,
    pub mount_path: String,
    pub sub_path: Option<String>,
    pub read_only: bool,
}

/// Reject ambiguous paths before applying owner-specific binding permissions.
pub fn validate_mounts(mounts: &[Mount]) -> Result<(), Invalid> {
    const RESERVED: &[&str] = &[
        "/proc",
        "/sys",
        "/dev",
        "/etc",
        "/run",
        "/var/run",
        "/home/worker/.ssh",
        "/home/worker/.config",
        "/opt/agent-platform",
    ];
    for (index, mount) in mounts.iter().enumerate() {
        let path = mount.mount_path.as_str();
        if path == "/"
            || !path.starts_with('/')
            || !clean_path(&path[1..])
            || RESERVED.iter().any(|reserved| overlaps(path, reserved))
            || mounts[..index]
                .iter()
                .any(|other| overlaps(path, &other.mount_path))
            || mount.sub_path.as_deref().is_some_and(|p| !clean_path(p))
        {
            return Err(Invalid::Mount);
        }
    }
    Ok(())
}

fn clean_path(path: &str) -> bool {
    path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.chars().any(|c| c.is_control() || c == '\\')
        })
}

fn overlaps(left: &str, right: &str) -> bool {
    left == right
        || left.strip_prefix(right).is_some_and(|s| s.starts_with('/'))
        || right.strip_prefix(left).is_some_and(|s| s.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ISSUER: &str = "https://sso.example.org/realms/agents";

    #[test]
    fn identity_keys_are_exact_and_email_is_not_a_key() {
        let key = Identity {
            issuer: ISSUER.into(),
            subject: "Owner-A".into(),
        };
        assert!(key.validate(ISSUER).is_ok());
        assert_ne!(
            key,
            Identity {
                subject: "owner-a".into(),
                ..key.clone()
            }
        );
        assert_eq!(key.validate(&ISSUER.to_uppercase()), Err(Invalid::Issuer));
        assert!(
            serde_json::from_str::<Identity>(
                r#"{"issuer":"https://sso.example.org","subject":"a","email":"a@example.org"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn reject_invalid_or_ambiguous_identity_strings() {
        for issuer in [
            "http://sso.example.org",
            "https://user@sso.example.org",
            "https://sso.example.org?x=1",
            "https://sso.example.org#fragment",
            " https://sso.example.org",
        ] {
            assert!(validate_issuer(issuer).is_err(), "{issuer}");
        }
        for subject in ["", " ", "a\nb", "ü"] {
            assert!(
                Identity {
                    issuer: ISSUER.into(),
                    subject: subject.into()
                }
                .validate(ISSUER)
                .is_err()
            );
        }
    }

    #[test]
    fn namespace_is_stable_bounded_and_owner_independent() {
        let installation = Uuid::new_v4();
        let instance = Uuid::new_v4();
        let name = instance_namespace(installation, instance);
        validate_name(&name).unwrap();
        assert!(name.len() <= 63);
        assert_eq!(name, instance_namespace(installation, instance));
        assert_ne!(name, instance_namespace(installation, Uuid::new_v4()));
        assert_eq!(Access::Enabled.desired_replicas(), 1);
        assert_eq!(Access::Revoked.desired_replicas(), 0);
    }

    #[test]
    fn mounts_cannot_replace_internal_paths_or_overlap() {
        let mount = Mount {
            storage_binding_id: Uuid::new_v4(),
            mount_path: "/mnt/home-network".into(),
            sub_path: None,
            read_only: true,
        };
        assert!(validate_mounts(std::slice::from_ref(&mount)).is_ok());
        for path in [
            "/",
            "/mnt/../etc",
            "/mnt//home",
            "/home/worker",
            "/home/worker/.ssh/keys",
            "/run/credentials",
            "/etc",
            "relative",
        ] {
            assert!(
                validate_mounts(&[Mount {
                    mount_path: path.into(),
                    ..mount.clone()
                }])
                .is_err(),
                "{path}"
            );
        }
        assert!(
            validate_mounts(&[
                mount.clone(),
                Mount {
                    mount_path: "/mnt/home-network/sub".into(),
                    ..mount.clone()
                }
            ])
            .is_err()
        );
        for sub_path in ["../home", "/home", ".", "a/../../b"] {
            assert!(
                validate_mounts(&[Mount {
                    sub_path: Some(sub_path.into()),
                    ..mount.clone()
                }])
                .is_err()
            );
        }
    }
}
