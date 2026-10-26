//! Exact OIDC identities. Email and display names never participate.

use serde::{Deserialize, Serialize};
use url::Url;

use super::{ErrorKind, Rejection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRejected {
    Malformed,
    ForeignIssuer,
}

impl Rejection for IdentityRejected {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Invalid
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Malformed => "invalid_identity",
            Self::ForeignIssuer => "foreign_issuer",
        }
    }
}

/// The exact `(issuer, subject)` pair. Comparison is byte-exact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
}

impl Identity {
    /// Checks the pair against the installation's configured issuer.
    pub fn validate(&self, configured_issuer: &str) -> Result<(), IdentityRejected> {
        validate_issuer(&self.issuer)?;
        if self.issuer != configured_issuer {
            return Err(IdentityRejected::ForeignIssuer);
        }
        let valid = !self.subject.is_empty()
            && self.subject.len() <= 255
            && self.subject.bytes().all(|b| b.is_ascii_graphic());
        valid.then_some(()).ok_or(IdentityRejected::Malformed)
    }
}

/// An HTTPS issuer URL without credentials, query or fragment.
pub fn validate_issuer(issuer: &str) -> Result<(), IdentityRejected> {
    let url = Url::parse(issuer).map_err(|_| IdentityRejected::Malformed)?;
    let valid = issuer.len() <= 2048
        && issuer.trim() == issuer
        && !issuer.chars().any(char::is_control)
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    valid.then_some(()).ok_or(IdentityRejected::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUER: &str = "https://sso.example.org/realms/agents";

    fn identity(subject: &str) -> Identity {
        Identity {
            issuer: ISSUER.into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn identity_keys_are_exact_and_email_is_not_a_key() {
        assert_eq!(identity("Owner-A").validate(ISSUER), Ok(()));
        assert_ne!(identity("Owner-A"), identity("owner-a"));
        assert_eq!(
            identity("a").validate(&ISSUER.to_uppercase()),
            Err(IdentityRejected::ForeignIssuer)
        );
        let with_email =
            r#"{"issuer":"https://sso.example.org","subject":"a","email":"a@example.org"}"#;
        assert!(serde_json::from_str::<Identity>(with_email).is_err());
    }

    #[test]
    fn invalid_or_ambiguous_identity_strings_are_rejected() {
        for issuer in [
            "http://sso.example.org",
            "https://user@example.org",
            "https://sso.example.org?x=1",
            "https://sso.example.org#fragment",
            " https://sso.example.org",
        ] {
            assert_eq!(
                validate_issuer(issuer),
                Err(IdentityRejected::Malformed),
                "{issuer}"
            );
        }
        for subject in ["", " ", "a\nb", "ü", &"a".repeat(256)] {
            assert_eq!(
                identity(subject).validate(ISSUER),
                Err(IdentityRejected::Malformed)
            );
        }
    }
}
