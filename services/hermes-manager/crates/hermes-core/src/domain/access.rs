//! Administrator grants. PostgreSQL grants are the only administrator authority.

use super::{ErrorKind, Rejection};

/// What a command requires of its actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Any active administrator grant.
    Administer,
    /// An active grant that may also manage other grants.
    ManageAdministrators,
    /// Only the operator CLI, never an HTTP caller.
    Operator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    pub active: bool,
    pub can_manage_administrators: bool,
    pub version: i64,
}

impl Grant {
    pub fn permits(&self, permission: Permission) -> bool {
        match permission {
            Permission::Administer => self.active,
            Permission::ManageAdministrators => self.manages(),
            Permission::Operator => false,
        }
    }

    pub fn manages(&self) -> bool {
        self.active && self.can_manage_administrators
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRejected {
    /// Bootstrap without `recover` while grants already exist.
    AdministratorsExist,
    AdministratorExists,
    IdentityNotVerified,
    LastAdministrator,
    StaleVersion,
    NotFound,
}

impl Rejection for AccessRejected {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::AdministratorsExist | Self::AdministratorExists | Self::LastAdministrator => {
                ErrorKind::Conflict
            }
            Self::IdentityNotVerified => ErrorKind::Invalid,
            Self::StaleVersion => ErrorKind::Precondition,
            Self::NotFound => ErrorKind::NotFound,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::AdministratorsExist => "administrators_exist",
            Self::AdministratorExists => "administrator_grant_exists",
            Self::IdentityNotVerified => "identity_not_verified",
            Self::LastAdministrator => "last_administrator",
            Self::StaleVersion => "stale_version",
            Self::NotFound => "not_found",
        }
    }
}

/// The first grant of an installation, or explicit operator recovery.
pub fn bootstrap(existing_grants: usize, recover: bool) -> Result<Grant, AccessRejected> {
    if existing_grants > 0 && !recover {
        return Err(AccessRejected::AdministratorsExist);
    }
    Ok(Grant {
        active: true,
        can_manage_administrators: true,
        version: 1,
    })
}

/// A new grant for an identity that has signed in at least once.
pub fn grant(
    verified: bool,
    existing: Option<Grant>,
    can_manage: bool,
) -> Result<Grant, AccessRejected> {
    if existing.is_some() {
        return Err(AccessRejected::AdministratorExists);
    }
    if !verified {
        return Err(AccessRejected::IdentityNotVerified);
    }
    Ok(Grant {
        active: true,
        can_manage_administrators: can_manage,
        version: 1,
    })
}

/// Changes a grant. `active_managers` counts every active managing grant,
/// including this one; no change may leave the installation without one.
pub fn change(
    current: Grant,
    expected_version: i64,
    active: bool,
    can_manage: bool,
    active_managers: usize,
) -> Result<Grant, AccessRejected> {
    if current.version != expected_version {
        return Err(AccessRejected::StaleVersion);
    }
    let next = Grant {
        active,
        can_manage_administrators: can_manage,
        version: current.version + 1,
    };
    if current.manages() && !next.manages() && active_managers <= 1 {
        return Err(AccessRejected::LastAdministrator);
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGER: Grant = Grant {
        active: true,
        can_manage_administrators: true,
        version: 3,
    };

    #[test]
    fn permissions_follow_the_grant() {
        let plain = Grant {
            can_manage_administrators: false,
            ..MANAGER
        };
        let inactive = Grant {
            active: false,
            ..MANAGER
        };
        assert!(MANAGER.permits(Permission::Administer));
        assert!(MANAGER.permits(Permission::ManageAdministrators));
        assert!(plain.permits(Permission::Administer));
        assert!(!plain.permits(Permission::ManageAdministrators));
        assert!(!inactive.permits(Permission::Administer));
        for grant in [MANAGER, plain, inactive] {
            assert!(!grant.permits(Permission::Operator));
        }
    }

    #[test]
    fn last_manager_cannot_be_removed_or_demoted() {
        for (active, can_manage) in [(false, true), (true, false), (false, false)] {
            assert_eq!(
                change(MANAGER, 3, active, can_manage, 1),
                Err(AccessRejected::LastAdministrator)
            );
            assert!(change(MANAGER, 3, active, can_manage, 2).is_ok());
        }
        let plain = Grant {
            can_manage_administrators: false,
            ..MANAGER
        };
        assert!(change(plain, 3, false, false, 1).is_ok());
    }

    #[test]
    fn grant_changes_are_versioned() {
        assert_eq!(
            change(MANAGER, 2, true, true, 2),
            Err(AccessRejected::StaleVersion)
        );
        assert_eq!(change(MANAGER, 3, true, true, 2).map(|g| g.version), Ok(4));
    }

    #[test]
    fn bootstrap_and_grants_require_their_preconditions() {
        assert!(bootstrap(0, false).is_ok());
        assert_eq!(
            bootstrap(1, false),
            Err(AccessRejected::AdministratorsExist)
        );
        assert!(bootstrap(1, true).is_ok());
        assert_eq!(
            grant(false, None, false),
            Err(AccessRejected::IdentityNotVerified)
        );
        assert_eq!(
            grant(true, Some(MANAGER), false),
            Err(AccessRejected::AdministratorExists)
        );
        assert_eq!(grant(true, None, true).map(|g| g.manages()), Ok(true));
    }
}
