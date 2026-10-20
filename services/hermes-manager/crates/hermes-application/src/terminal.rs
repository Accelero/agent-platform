//! Owner-scoped terminal clients and durable lease lifecycle. No Kubernetes or
//! transport access, credential values in persistence, or request-selected specs.
pub mod clients;
pub mod leases;

use crate::Error;

pub fn identifier(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 256
        || !value.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
