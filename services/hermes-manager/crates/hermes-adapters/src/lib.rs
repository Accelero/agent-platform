//! Infrastructure implementations. No adapter is imported by domain/application.
pub mod auth;
pub mod auth_config;
pub mod browser_auth;
pub mod browser_http;
pub mod http;
pub mod kubernetes;
pub mod materials;
pub mod native_bootstrap;
pub mod native_receipt;
pub mod oidc;
pub mod ownership;
pub mod postgres;
pub mod router_auth;
pub mod server;
pub mod session_crypto;
pub mod sessions;
pub mod workloads;
