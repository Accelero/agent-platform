//! Application errors: a reported refusal, or a store failure.

use thiserror::Error;

use crate::domain::{ErrorKind, Problem, Rejection};

/// A refusal reported to the caller with a stable code, and for invalid
/// configuration the problems found, each at a JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}")]
pub struct AppError {
    pub kind: ErrorKind,
    pub code: &'static str,
    pub problems: Vec<Problem>,
}

impl AppError {
    pub const fn new(kind: ErrorKind, code: &'static str) -> Self {
        Self {
            kind,
            code,
            problems: Vec::new(),
        }
    }

    /// `invalid_configuration` with the problems that make it invalid.
    pub fn invalid_configuration(problems: Vec<Problem>) -> Self {
        Self {
            problems,
            ..Self::new(ErrorKind::Invalid, "invalid_configuration")
        }
    }

    pub const fn forbidden() -> Self {
        Self::new(ErrorKind::Forbidden, "access_denied")
    }

    pub const fn invalid() -> Self {
        Self::new(ErrorKind::Invalid, "invalid_request")
    }

    pub const fn unavailable() -> Self {
        Self::new(ErrorKind::Unavailable, "dependency_unavailable")
    }

    /// The registry belongs to another installation or is not initialized.
    pub const fn registry_mismatch() -> Self {
        Self::new(ErrorKind::Unavailable, "registry_unavailable")
    }

    pub const fn idempotency_conflict() -> Self {
        Self::new(ErrorKind::Conflict, "idempotency_conflict")
    }

    pub const fn concurrent_change() -> Self {
        Self::new(ErrorKind::Conflict, "concurrent_change")
    }

    pub const fn fault_injected() -> Self {
        Self::new(ErrorKind::Unavailable, "fault_injected")
    }
}

impl<R: Rejection> From<R> for AppError {
    fn from(rejection: R) -> Self {
        Self::new(rejection.kind(), rejection.code())
    }
}

/// A store port failure. Carries no SQL text, URLs or row contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StoreError {
    /// Serialization failure, deadlock or lost race: the transaction may be retried.
    #[error("concurrent change")]
    Conflict,
    /// The database is unreachable, timed out or returned unusable data.
    #[error("store unavailable")]
    Unavailable,
}

/// Why a command did not apply: refused, or the store failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Rejected(AppError),
    Store(StoreError),
}

impl From<StoreError> for Failure {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<AppError> for Failure {
    fn from(error: AppError) -> Self {
        Self::Rejected(error)
    }
}

impl<R: Rejection> From<R> for Failure {
    fn from(rejection: R) -> Self {
        Self::Rejected(rejection.into())
    }
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Conflict => Self::concurrent_change(),
            StoreError::Unavailable => Self::unavailable(),
        }
    }
}
