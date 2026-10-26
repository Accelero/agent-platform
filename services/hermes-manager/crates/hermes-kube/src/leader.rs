//! Leader election on a `coordination.k8s.io/v1` Lease: 15 s duration,
//! renewed every few seconds. Writes are optimistic on the resourceVersion,
//! so two candidates never both win a term.

use std::time::Duration;

use hermes_core::app::controller::ClusterError;
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kube::api::PostParams;
use kube::{Api, Client};

use crate::timed;

pub const LEASE_DURATION: Duration = Duration::from_secs(15);
/// A leader that has not renewed for this long stops writing.
pub const RENEW_DEADLINE: Duration = Duration::from_secs(10);
pub const RETRY_PERIOD: Duration = Duration::from_secs(2);

pub struct LeaderElection {
    leases: Api<Lease>,
    name: String,
    identity: String,
}

impl LeaderElection {
    pub fn new(client: Client, namespace: &str, name: &str, identity: &str) -> Self {
        Self {
            leases: Api::namespaced(client, namespace),
            name: name.to_owned(),
            identity: identity.to_owned(),
        }
    }

    fn spec(&self, acquired: Option<MicroTime>, transitions: i32) -> LeaseSpec {
        let now = MicroTime(Timestamp::now());
        LeaseSpec {
            holder_identity: Some(self.identity.clone()),
            lease_duration_seconds: Some(LEASE_DURATION.as_secs() as i32),
            acquire_time: Some(acquired.unwrap_or_else(|| now.clone())),
            renew_time: Some(now),
            lease_transitions: Some(transitions),
            ..LeaseSpec::default()
        }
    }

    /// Acquires or renews the lease. `Ok(true)` while this candidate leads.
    pub async fn try_lead(&self) -> Result<bool, ClusterError> {
        let Some(mut lease) = timed(self.leases.get_opt(&self.name)).await? else {
            let lease = Lease {
                metadata: ObjectMeta {
                    name: Some(self.name.clone()),
                    ..ObjectMeta::default()
                },
                spec: Some(self.spec(None, 0)),
            };
            return match timed(self.leases.create(&PostParams::default(), &lease)).await {
                Ok(_) => Ok(true),
                Err(ClusterError::Conflict) => Ok(false),
                Err(error) => Err(error),
            };
        };
        let spec = lease.spec.clone().unwrap_or_default();
        let ours = spec.holder_identity.as_deref() == Some(self.identity.as_str());
        let expired = spec.renew_time.as_ref().is_none_or(|renewed| {
            let duration = i64::from(
                spec.lease_duration_seconds
                    .unwrap_or(LEASE_DURATION.as_secs() as i32),
            );
            renewed.0 + SignedDuration::from_secs(duration) < Timestamp::now()
        });
        if !ours && !expired {
            return Ok(false);
        }
        let transitions = spec.lease_transitions.unwrap_or(0) + i32::from(!ours);
        lease.spec = Some(self.spec(if ours { spec.acquire_time } else { None }, transitions));
        // `replace` carries the observed resourceVersion: a concurrent
        // takeover makes this write fail instead of both winning.
        match timed(
            self.leases
                .replace(&self.name, &PostParams::default(), &lease),
        )
        .await
        {
            Ok(_) => Ok(true),
            Err(ClusterError::Conflict) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Gives the lease up on shutdown so a successor need not wait.
    pub async fn release(&self) -> Result<(), ClusterError> {
        let Some(mut lease) = timed(self.leases.get_opt(&self.name)).await? else {
            return Ok(());
        };
        let Some(spec) = lease.spec.as_mut() else {
            return Ok(());
        };
        if spec.holder_identity.as_deref() != Some(self.identity.as_str()) {
            return Ok(());
        }
        spec.holder_identity = None;
        spec.renew_time = None;
        timed(
            self.leases
                .replace(&self.name, &PostParams::default(), &lease),
        )
        .await
        .map(|_| ())
    }
}
