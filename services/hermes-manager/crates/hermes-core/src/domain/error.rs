//! The shared shape of every refusal.

/// How a refusal is reported. Transports map each kind to one status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Malformed, unapproved or unsupported input.
    Invalid,
    /// No verified actor.
    Unauthenticated,
    /// The actor lacks the permission.
    Forbidden,
    NotFound,
    /// The request conflicts with current state or an earlier request.
    Conflict,
    /// An optimistic version precondition no longer holds.
    Precondition,
    /// Accepted work is not ready yet; retry with the same request.
    Pending,
    /// A dependency is unavailable or the installation does not match.
    Unavailable,
}

/// A typed refusal with a stable code that contracts publish.
pub trait Rejection {
    fn kind(&self) -> ErrorKind;
    fn code(&self) -> &'static str;
}

/// One finding in submitted configuration, located by JSON pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub pointer: String,
    pub code: &'static str,
}

/// Collects problems while validating a document.
#[derive(Debug, Default)]
pub struct Problems(Vec<Problem>);

impl Problems {
    pub fn add(&mut self, pointer: impl Into<String>, code: &'static str) {
        self.0.push(Problem {
            pointer: pointer.into(),
            code,
        });
    }

    /// Adds a problem at `pointer` unless `valid`.
    pub fn check(&mut self, valid: bool, pointer: impl Into<String>, code: &'static str) {
        if !valid {
            self.add(pointer, code);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_result(self) -> Result<(), Vec<Problem>> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(self.0)
        }
    }

    pub fn into_vec(self) -> Vec<Problem> {
        self.0
    }
}

/// Escapes a key for use as one JSON pointer segment.
pub fn pointer_segment(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
