//! Domain model and application layer of the personal Hermes manager.
//!
//! `domain` holds aggregates, typed states, planners and validators. It is
//! synchronous and performs no I/O. `app` holds commands, queries, ports and
//! the controller loop; every external effect goes through a port trait.
pub mod app;
pub mod domain;
