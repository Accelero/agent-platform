//! Instance-side cryptography and probes: sealing of admin secrets and
//! instance material, the controller's vault, init-receipt verification and
//! the native auth-gate probe.

pub mod materials;
pub mod probe;
pub mod receipt;
pub mod sealing;
pub mod ssh;
