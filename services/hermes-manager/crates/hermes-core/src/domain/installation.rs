//! The installation this process belongs to, fixed by configuration.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::identity::{IdentityRejected, validate_issuer};

/// Stable installation identity. Every database transaction checks that the
/// registry belongs to this installation before acting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installation {
    pub id: Uuid,
    pub issuer: String,
}

impl Installation {
    pub fn validate(&self) -> Result<(), IdentityRejected> {
        if self.id.is_nil() {
            return Err(IdentityRejected::Malformed);
        }
        validate_issuer(&self.issuer)
    }

    /// Namespace prefix of every instance namespace: `hi-` plus 12 hex digits of
    /// the installation ID, so the full name stays within 63 bytes.
    pub fn namespace_prefix(&self) -> String {
        format!("hi-{}", &self.id.simple().to_string()[..12])
    }

    /// Stable, personal-data-free namespace for one instance.
    pub fn instance_namespace(&self, instance: Uuid) -> String {
        format!("{}-{}", self.namespace_prefix(), instance.simple())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::names::validate_name;

    #[test]
    fn namespace_is_stable_bounded_and_owner_independent() {
        let installation = Installation {
            id: Uuid::new_v4(),
            issuer: "https://sso.example.org/realms/agents".into(),
        };
        let instance = Uuid::new_v4();
        let name = installation.instance_namespace(instance);
        assert_eq!(validate_name(&name), Ok(()));
        assert_eq!(name, installation.instance_namespace(instance));
        assert_ne!(name, installation.instance_namespace(Uuid::new_v4()));
        assert!(name.starts_with(&installation.namespace_prefix()));
    }

    #[test]
    fn installation_needs_an_id_and_an_https_issuer() {
        let mut installation = Installation {
            id: Uuid::nil(),
            issuer: "https://sso.example.org".into(),
        };
        assert!(installation.validate().is_err());
        installation.id = Uuid::new_v4();
        assert!(installation.validate().is_ok());
        installation.issuer = "http://sso.example.org".into();
        assert!(installation.validate().is_err());
    }
}
