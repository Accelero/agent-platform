//! Exact-owner authorization, separate from the plugin and cryptographic adapter.
use std::collections::BTreeSet;

use async_trait::async_trait;
use hermes_domain::Identity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, InstallationScope};

/// A cryptographic adapter constructs this only AFTER checking the pinned
/// signature algorithm/key, issuer, token type, audience and lifetime. Unlike an
/// ID token, this is user access-token evidence for the dedicated route API.
#[derive(Clone)]
pub struct VerifiedRouteEvidence {
    pub identity: Identity,
    pub audiences: BTreeSet<String>,
    pub authorized_client: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Constructed from the validated mTLS client certificate SAN by the listener;
/// never from a forwarded HTTP identity header or a browser request field.
#[derive(Clone)]
pub struct VerifiedRouterService {
    pub identity: String,
}

#[derive(Clone)]
pub struct RoutePolicy {
    pub issuer: String,
    pub audience: String,
    pub authorized_client: String,
    pub router_service_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisteredDestination {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    pub scheme: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteDecision {
    Ready {
        #[serde(rename = "instanceId")]
        instance_id: Uuid,
        #[serde(rename = "routeGeneration")]
        route_generation: i64,
        #[serde(rename = "authorizationVersion")]
        authorization_version: i64,
        destination: RegisteredDestination,
    },
    Pending {
        reason: PendingReason,
    },
    Denied {
        reason: DeniedReason,
    },
    Unavailable {
        reason: UnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    InstanceNotReady,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedReason {
    AccessDenied,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    DependencyUnavailable,
    Maintenance,
    RegistryUnavailable,
}

#[async_trait]
pub trait RouteRegistry: Send + Sync {
    /// Must read current assignment authorization on every call. This port may
    /// not commission, enroll, retry or cache positive authorization decisions.
    async fn resolve(
        &self,
        scope: &InstallationScope,
        identity: &Identity,
    ) -> Result<RouteDecision, Error>;
}

pub struct Routes<S> {
    registry: S,
    scope: InstallationScope,
    policy: RoutePolicy,
}

impl<S: RouteRegistry> Routes<S> {
    pub fn new(registry: S, scope: InstallationScope, policy: RoutePolicy) -> Result<Self, Error> {
        hermes_domain::validate_issuer(&scope.issuer)?;
        if scope.id.is_nil()
            || scope.epoch < 1
            || scope.issuer != policy.issuer
            || policy.audience.is_empty()
            || policy.authorized_client.is_empty()
            || policy.router_service_identity.is_empty()
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            registry,
            scope,
            policy,
        })
    }

    pub async fn resolve(
        &self,
        service: &VerifiedRouterService,
        evidence: &VerifiedRouteEvidence,
        requested: &Identity,
        now: i64,
    ) -> Result<RouteDecision, Error> {
        if service.identity != self.policy.router_service_identity {
            return Err(Error::Forbidden);
        }
        if requested.validate(&self.policy.issuer).is_err()
            || evidence.identity != *requested
            || !evidence.audiences.contains(&self.policy.audience)
            || evidence.authorized_client != self.policy.authorized_client
            || evidence.issued_at > now
            || now.saturating_sub(evidence.issued_at) > 120
            || evidence.expires_at <= now
            || evidence.expires_at <= evidence.issued_at
        {
            return Err(Error::InvalidIdentityEvidence);
        }
        Ok(match self.registry.resolve(&self.scope, requested).await {
            Ok(decision) => decision,
            Err(Error::Maintenance) => RouteDecision::Unavailable {
                reason: UnavailableReason::Maintenance,
            },
            Err(Error::RecoveryRequired) => RouteDecision::Unavailable {
                reason: UnavailableReason::RegistryUnavailable,
            },
            Err(_) => RouteDecision::Unavailable {
                reason: UnavailableReason::DependencyUnavailable,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone, Default)]
    struct Registry(Arc<AtomicUsize>);
    #[async_trait]
    impl RouteRegistry for Registry {
        async fn resolve(
            &self,
            _: &InstallationScope,
            _: &Identity,
        ) -> Result<RouteDecision, Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(RouteDecision::Denied {
                reason: DeniedReason::AccessDenied,
            })
        }
    }

    #[tokio::test]
    async fn mtls_and_fresh_exact_audience_client_bound_identity_are_both_required() {
        let issuer = "https://sso.example.org/realms/agents";
        let registry = Registry::default();
        let routes = Routes::new(
            registry.clone(),
            InstallationScope {
                id: Uuid::new_v4(),
                epoch: 1,
                issuer: issuer.into(),
            },
            RoutePolicy {
                issuer: issuer.into(),
                audience: "route-api".into(),
                authorized_client: "hermes-router".into(),
                router_service_identity: "spiffe://agents.example.org/router-plugin".into(),
            },
        )
        .unwrap();
        let service = VerifiedRouterService {
            identity: "spiffe://agents.example.org/router-plugin".into(),
        };
        let identity = Identity {
            issuer: issuer.into(),
            subject: "owner-a".into(),
        };
        let evidence = VerifiedRouteEvidence {
            identity: identity.clone(),
            audiences: BTreeSet::from(["route-api".into()]),
            authorized_client: "hermes-router".into(),
            issued_at: 1000,
            expires_at: 2000,
        };
        assert_eq!(
            routes
                .resolve(
                    &VerifiedRouterService {
                        identity: "different-service".into()
                    },
                    &evidence,
                    &identity,
                    1100
                )
                .await,
            Err(Error::Forbidden)
        );
        for time in [999, 1121, 2000] {
            assert_eq!(
                routes.resolve(&service, &evidence, &identity, time).await,
                Err(Error::InvalidIdentityEvidence)
            );
        }
        let mut invalid = evidence.clone();
        invalid.identity.subject = "owner-b".into();
        assert_eq!(
            routes.resolve(&service, &invalid, &identity, 1100).await,
            Err(Error::InvalidIdentityEvidence)
        );
        invalid = evidence.clone();
        invalid.audiences = BTreeSet::from(["admin-api".into()]);
        assert_eq!(
            routes.resolve(&service, &invalid, &identity, 1100).await,
            Err(Error::InvalidIdentityEvidence)
        );
        invalid = evidence.clone();
        invalid.authorized_client = "native-hermes".into();
        assert_eq!(
            routes.resolve(&service, &invalid, &identity, 1100).await,
            Err(Error::InvalidIdentityEvidence)
        );
        assert_eq!(registry.0.load(Ordering::SeqCst), 0);
        for _ in 0..2 {
            assert!(matches!(
                routes.resolve(&service, &evidence, &identity, 1120).await,
                Ok(RouteDecision::Denied { .. })
            ));
        }
        assert_eq!(registry.0.load(Ordering::SeqCst), 2);
    }
}
