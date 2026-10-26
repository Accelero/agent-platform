//! Authentication failures. Never carries provider responses or token contents.

use hermes_core::domain::{ErrorKind, Rejection};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("invalid authentication configuration")]
    Configuration,
    #[error("invalid identity evidence")]
    Evidence,
    #[error("access denied")]
    Forbidden,
    #[error("identity provider or session store unavailable")]
    Unavailable,
}

impl Rejection for AuthError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Configuration => ErrorKind::Invalid,
            Self::Evidence => ErrorKind::Unauthenticated,
            Self::Forbidden => ErrorKind::Forbidden,
            Self::Unavailable => ErrorKind::Unavailable,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Configuration => "invalid_request",
            Self::Evidence => "invalid_identity_evidence",
            Self::Forbidden => "access_denied",
            Self::Unavailable => "dependency_unavailable",
        }
    }
}
