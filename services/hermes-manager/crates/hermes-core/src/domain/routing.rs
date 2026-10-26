//! Route resolution (`router.openapi.yaml`): which registered destination, if
//! any, a verified identity reaches. There is no fallback: anything but an
//! enabled assignment with a converged, ready instance is not a route.

use uuid::Uuid;

use super::assignments::Access;
use super::installation::Installation;
use super::instances::Phase;

/// The instance Service every route points at.
pub const SERVICE: &str = "hermes";
pub const SERVICE_PORT: u16 = 8080;

/// What the registry holds for one identity's assignment and instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub instance_id: Uuid,
    pub namespace: String,
    /// The namespace UID is recorded, so the namespace is the instance's own.
    pub namespace_bound: bool,
    pub access: Access,
    pub phase: Phase,
    pub desired_generation: i64,
    pub observed_generation: i64,
    pub assignment_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub namespace: String,
    pub service: &'static str,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub instance_id: Uuid,
    /// The desired generation; a change ends cached decisions and open
    /// connections.
    pub route_generation: i64,
    pub authorization_version: i64,
    pub destination: Destination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingReason {
    InstanceNotReady,
    /// Enabled but blocked; the owner's status page says an administrator
    /// is needed.
    InstanceBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    DependencyUnavailable,
    /// The registry row cannot be a route of this installation.
    RegistryUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Ready(Route),
    Pending(PendingReason),
    Denied,
    Unavailable(UnavailableReason),
}

/// The decision for an identity whose registry row is `row`.
pub fn decide(installation: &Installation, row: Option<&RouteRow>) -> Decision {
    let Some(row) = row else {
        return Decision::Denied;
    };
    match row.access {
        Access::Enabled => {}
        Access::Suspended | Access::Revoked => return Decision::Denied,
    }
    if row.namespace != installation.instance_namespace(row.instance_id)
        || row.observed_generation > row.desired_generation
        || row.desired_generation < 1
        || row.assignment_version < 1
    {
        return Decision::Unavailable(UnavailableReason::RegistryUnavailable);
    }
    match row.phase {
        Phase::Ready if !row.namespace_bound => {
            Decision::Unavailable(UnavailableReason::RegistryUnavailable)
        }
        Phase::Ready if row.observed_generation == row.desired_generation => {
            Decision::Ready(Route {
                instance_id: row.instance_id,
                route_generation: row.desired_generation,
                authorization_version: row.assignment_version,
                destination: Destination {
                    namespace: row.namespace.clone(),
                    service: SERVICE,
                    port: SERVICE_PORT,
                },
            })
        }
        Phase::Blocked => Decision::Pending(PendingReason::InstanceBlocked),
        Phase::Ready | Phase::Provisioning | Phase::Stopping | Phase::Retained => {
            Decision::Pending(PendingReason::InstanceNotReady)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installation() -> Installation {
        Installation {
            id: Uuid::new_v4(),
            issuer: "https://sso.example.org/realms/agents".into(),
        }
    }

    fn ready(installation: &Installation) -> RouteRow {
        let instance_id = Uuid::new_v4();
        RouteRow {
            instance_id,
            namespace: installation.instance_namespace(instance_id),
            namespace_bound: true,
            access: Access::Enabled,
            phase: Phase::Ready,
            desired_generation: 3,
            observed_generation: 3,
            assignment_version: 2,
        }
    }

    #[test]
    fn only_an_enabled_converged_ready_instance_is_a_route() {
        let installation = installation();
        let row = ready(&installation);
        assert_eq!(
            decide(&installation, Some(&row)),
            Decision::Ready(Route {
                instance_id: row.instance_id,
                route_generation: 3,
                authorization_version: 2,
                destination: Destination {
                    namespace: row.namespace.clone(),
                    service: "hermes",
                    port: 8080,
                },
            })
        );
        assert_eq!(decide(&installation, None), Decision::Denied);
        for access in [Access::Suspended, Access::Revoked] {
            let row = RouteRow {
                access,
                ..ready(&installation)
            };
            assert_eq!(decide(&installation, Some(&row)), Decision::Denied);
        }
        let stale = RouteRow {
            desired_generation: 4,
            ..row.clone()
        };
        assert_eq!(
            decide(&installation, Some(&stale)),
            Decision::Pending(PendingReason::InstanceNotReady)
        );
        for phase in [Phase::Provisioning, Phase::Stopping, Phase::Retained] {
            let row = RouteRow {
                phase,
                ..row.clone()
            };
            assert_eq!(
                decide(&installation, Some(&row)),
                Decision::Pending(PendingReason::InstanceNotReady)
            );
        }
        let blocked = RouteRow {
            phase: Phase::Blocked,
            ..row.clone()
        };
        assert_eq!(
            decide(&installation, Some(&blocked)),
            Decision::Pending(PendingReason::InstanceBlocked)
        );
    }

    #[test]
    fn registry_rows_outside_the_installation_are_never_routes() {
        let installation = installation();
        let row = ready(&installation);
        let foreign = RouteRow {
            namespace: self::installation().instance_namespace(row.instance_id),
            ..row.clone()
        };
        let unbound = RouteRow {
            namespace_bound: false,
            ..row.clone()
        };
        let ahead = RouteRow {
            observed_generation: 4,
            ..row.clone()
        };
        let other_instance = RouteRow {
            instance_id: Uuid::new_v4(),
            ..row.clone()
        };
        for row in [foreign, unbound, ahead, other_instance] {
            assert_eq!(
                decide(&installation, Some(&row)),
                Decision::Unavailable(UnavailableReason::RegistryUnavailable)
            );
        }
        // A disabled assignment is denied before its row is inspected.
        let revoked_foreign = RouteRow {
            access: Access::Revoked,
            namespace: "elsewhere".into(),
            ..row
        };
        assert_eq!(
            decide(&installation, Some(&revoked_foreign)),
            Decision::Denied
        );
    }
}
