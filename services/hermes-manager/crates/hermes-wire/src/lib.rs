//! Wire shapes of the manager's HTTP contracts in
//! `contracts/hermes-manager/`: request bodies, error and problem bodies,
//! and the strict JSON parser every request body goes through. Query views
//! from `hermes_core::app::queries` serialize directly as response bodies; the
//! contract tests check them against the schemas.

pub mod admin;
pub mod routing;
pub mod strict;

use hermes_core::domain::Problem;
use serde::Serialize;

/// One configuration finding, as the contract publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemBody {
    pub pointer: String,
    pub code: &'static str,
    pub severity: &'static str,
}

impl From<&Problem> for ProblemBody {
    fn from(problem: &Problem) -> Self {
        Self {
            pointer: problem.pointer.clone(),
            code: problem.code,
            severity: "error",
        }
    }
}

pub fn problems(problems: &[Problem]) -> Vec<ProblemBody> {
    problems.iter().take(64).map(ProblemBody::from).collect()
}

/// The body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub error: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<ProblemBody>,
}
