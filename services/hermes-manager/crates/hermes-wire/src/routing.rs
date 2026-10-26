//! Route resolution (`router.openapi.yaml`) bodies. The manager serializes
//! them; the gateway parses the same types strictly and accepts a destination
//! only through [`DecisionBody::route_for`].

use hermes_core::domain::identity::Identity;
use hermes_core::domain::installation::Installation;
use hermes_core::domain::routing::{
    Decision, PendingReason, Route, SERVICE, SERVICE_PORT, UnavailableReason,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub identity: Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationBody {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    pub scheme: Scheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingCode {
    InstanceNotReady,
    InstanceBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedCode {
    AccessDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableCode {
    DependencyUnavailable,
    RegistryUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
pub enum DecisionBody {
    #[serde(rename_all = "camelCase")]
    Ready {
        instance_id: Uuid,
        route_generation: i64,
        authorization_version: i64,
        destination: DestinationBody,
    },
    Pending {
        reason: PendingCode,
    },
    Denied {
        reason: DeniedCode,
    },
    Unavailable {
        reason: UnavailableCode,
    },
}

impl DecisionBody {
    /// The caller is not the gateway.
    pub const fn denied() -> Self {
        Self::Denied {
            reason: DeniedCode::AccessDenied,
        }
    }

    pub const fn unavailable() -> Self {
        Self::Unavailable {
            reason: UnavailableCode::DependencyUnavailable,
        }
    }

    /// The route of a ready decision, if its destination is exactly the
    /// instance Service in the instance's own namespace of `installation`.
    pub fn route_for(&self, installation: &Installation) -> Option<Route> {
        let Self::Ready {
            instance_id,
            route_generation,
            authorization_version,
            destination,
        } = self
        else {
            return None;
        };
        let exact = !instance_id.is_nil()
            && *route_generation >= 1
            && *authorization_version >= 1
            && destination.namespace == installation.instance_namespace(*instance_id)
            && destination.service == SERVICE
            && destination.port == SERVICE_PORT
            && destination.scheme == Scheme::Http;
        exact.then(|| Route {
            instance_id: *instance_id,
            route_generation: *route_generation,
            authorization_version: *authorization_version,
            destination: hermes_core::domain::routing::Destination {
                namespace: destination.namespace.clone(),
                service: SERVICE,
                port: SERVICE_PORT,
            },
        })
    }
}

impl From<&Decision> for DecisionBody {
    fn from(decision: &Decision) -> Self {
        match decision {
            Decision::Ready(route) => Self::Ready {
                instance_id: route.instance_id,
                route_generation: route.route_generation,
                authorization_version: route.authorization_version,
                destination: DestinationBody {
                    namespace: route.destination.namespace.clone(),
                    service: route.destination.service.to_owned(),
                    port: route.destination.port,
                    scheme: Scheme::Http,
                },
            },
            Decision::Pending(reason) => Self::Pending {
                reason: match reason {
                    PendingReason::InstanceNotReady => PendingCode::InstanceNotReady,
                    PendingReason::InstanceBlocked => PendingCode::InstanceBlocked,
                },
            },
            Decision::Denied => Self::denied(),
            Decision::Unavailable(reason) => Self::Unavailable {
                reason: match reason {
                    UnavailableReason::DependencyUnavailable => {
                        UnavailableCode::DependencyUnavailable
                    }
                    UnavailableReason::RegistryUnavailable => UnavailableCode::RegistryUnavailable,
                },
            },
        }
    }
}
