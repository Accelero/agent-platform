//! Application layer: commands, queries, ports and the controller loop.
//!
//! Every desired-state write is a [`envelope::Command`] executed by the
//! [`envelope::CommandEnvelope`]. Ports in [`ports`] are the only way to reach
//! storage; adapters implement them.
pub mod access;
pub mod actor;
pub mod assignments;
pub mod catalog;
pub mod controller;
pub mod directory;
pub mod envelope;
pub mod error;
pub mod execution;
pub mod faults;
pub mod ports;
pub mod queries;
pub mod routing;
pub mod secrets;
pub mod settings;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
