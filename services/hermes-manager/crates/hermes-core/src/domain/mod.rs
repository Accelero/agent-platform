//! Pure domain model. No async, no I/O, no clock: time is passed in as values.
//!
//! Every refusal is a typed rejection with a stable code (see [`Rejection`]).
pub mod access;
pub mod assignments;
pub mod backends;
pub mod catalog;
pub mod error;
pub mod execution;
pub mod identity;
pub mod installation;
pub mod instances;
pub mod materials;
pub mod names;
pub mod orphans;
pub mod routing;
pub mod secrets;
pub mod settings;
pub mod storage;
pub mod templates;

pub use error::{ErrorKind, Problem, Problems, Rejection, pointer_segment};
