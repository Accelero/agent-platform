//! Process roles. Each builds its adapters from configuration and runs.
pub mod api;
pub mod controller;
pub mod operator;

pub use hermes_http::server::shutdown_signal;
