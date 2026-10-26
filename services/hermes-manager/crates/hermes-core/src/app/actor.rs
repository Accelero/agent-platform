//! Who is acting. Produced only by authentication adapters or the operator CLI.

use crate::domain::identity::Identity;

/// The verified actor of a command. HTTP adapters construct `Administrator`
/// only from a cryptographically verified token or session; only the operator
/// CLI constructs `Operator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Administrator(Identity),
    Operator,
}

impl Actor {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Administrator(_) => "administrator",
            Self::Operator => "operator",
        }
    }

    pub fn identity(&self) -> Option<&Identity> {
        match self {
            Self::Administrator(identity) => Some(identity),
            Self::Operator => None,
        }
    }
}
