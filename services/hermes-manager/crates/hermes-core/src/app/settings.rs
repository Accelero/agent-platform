//! Installation settings every command sees: identity, approved storage
//! sources and the data identity. Fixed by configuration for the process
//! lifetime; never changed by an API call.

use crate::domain::installation::Installation;
use crate::domain::storage::{DataIdentity, StorageSources};

#[derive(Debug, Clone)]
pub struct Settings {
    pub installation: Installation,
    pub sources: StorageSources,
    pub data_identity: DataIdentity,
}

impl Settings {
    /// Settings with no storage sources and the default data identity.
    pub fn minimal(installation: Installation) -> Self {
        Self {
            installation,
            sources: StorageSources::default(),
            data_identity: DataIdentity::default(),
        }
    }
}
