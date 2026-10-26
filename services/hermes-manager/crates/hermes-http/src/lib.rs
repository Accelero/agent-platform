//! HTTP adapters over the application layer: the administration API and its
//! browser sign-in, the execution API, the route resolver, and the shared TLS
//! listener. Handlers translate wire types
//! and call one use case each; they never reach a store or cluster directly.

pub mod admin;
mod body;
pub mod browser;
pub mod cookies;
pub mod error;
pub mod execution;
pub mod files;
pub mod limits;
pub mod resolver;
pub mod server;
