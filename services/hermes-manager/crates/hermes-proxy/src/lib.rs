//! Gateway core: request hygiene, platform sessions, route resolution over
//! mTLS, forwarding to the owner's instance, and revocation of established
//! WebSockets and streams.

pub mod cache;
pub mod gateway;
pub mod hygiene;
pub mod pages;
pub mod resolver;
pub mod sessions;
pub mod supervise;
