//! Test support shared by every crate: the in-memory store, scripted faults,
//! fixtures and the port contract suites. Enabled by the `testing` feature.
//!
//! Each contract case is a plain async function generic over the store. The
//! [`contract_tests!`](crate::contract_tests) macro instantiates every case for
//! one store, so fakes and real adapters run the same cases.

pub mod contracts;
pub mod fakes;
pub mod memory;

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Mutex;
use std::task::Poll;

use async_trait::async_trait;
use uuid::Uuid;

use super::faults::{FaultHook, FaultPoint, Injected};
use crate::domain::access::Grant;
use crate::domain::identity::Identity;
use crate::domain::installation::Installation;

pub const ISSUER: &str = "https://sso.example.org/realms/agents";

/// The fixture installation every contract store is initialized for.
pub fn installation() -> Installation {
    Installation {
        id: Uuid::from_u128(0x7d6c_0e1a_0000_4000_8000_0000_0000_0001),
        issuer: ISSUER.to_owned(),
    }
}

pub fn identity(subject: &str) -> Identity {
    Identity {
        issuer: ISSUER.to_owned(),
        subject: subject.to_owned(),
    }
}

/// Fixture settings: the fixture installation, an NVMe storage class, a local
/// path and an NFS export, and the default data identity.
pub fn settings() -> std::sync::Arc<crate::app::settings::Settings> {
    use crate::domain::storage::{DataIdentity, SourceKind, StorageSource, StorageSources};
    let sources = StorageSources::new(vec![
        StorageSource {
            id: "nvme".into(),
            kind: SourceKind::StorageClass {
                name: "local-nvme".into(),
            },
        },
        StorageSource {
            id: "host-data".into(),
            kind: SourceKind::LocalPath {
                node: "server-1".into(),
                root: "/srv/hermes-data".into(),
            },
        },
        StorageSource {
            id: "nas".into(),
            kind: SourceKind::Nfs {
                server: "nas.internal".into(),
                root: "/export/homes".into(),
            },
        },
    ])
    .expect("fixture sources");
    std::sync::Arc::new(crate::app::settings::Settings {
        installation: installation(),
        sources,
        data_identity: DataIdentity::default(),
    })
}

/// Read-only views and setup steps that contract cases need beyond the ports.
#[async_trait]
pub trait Inspect: Send + Sync {
    async fn audit_actions(&self) -> Vec<String>;
    async fn receipt_count(&self) -> usize;
    async fn grant_of(&self, identity: &Identity) -> Option<Grant>;
    async fn active_managers(&self) -> usize;
    /// `(desired, execution)` generations of an instance.
    async fn instance_generations(&self, instance: Uuid) -> (i64, i64);
    async fn instance_namespace(&self, instance: Uuid) -> String;
    /// Every stored `(version, sealed value)` of a secret.
    async fn secret_versions(&self, name: &str) -> Vec<(i64, Vec<u8>)>;
    /// Records a past successful login, as the identity adapter will.
    async fn mark_verified(&self, identity: &Identity);
    /// `(phase, blocked reason, observed generation)` as recorded.
    async fn instance_status(&self, instance: Uuid) -> (String, Option<String>, i64);
    /// Moves database time forward by `seconds` (backwards when negative)
    /// for every lease and environment timestamp.
    async fn advance_time(&self, seconds: i64);
    /// The recorded orphan report, sorted.
    async fn orphans(&self) -> Vec<crate::domain::orphans::Orphan>;
}

/// Fails once at each listed point, then behaves normally.
#[derive(Default)]
pub struct ScriptedFaults {
    pending: Mutex<Vec<FaultPoint>>,
}

impl ScriptedFaults {
    pub fn once(point: FaultPoint) -> Self {
        Self {
            pending: Mutex::new(vec![point]),
        }
    }
}

impl FaultHook for ScriptedFaults {
    fn hit(&self, point: FaultPoint) -> Result<(), Injected> {
        let mut pending = self.pending.lock().expect("faults lock");
        match pending.iter().position(|p| *p == point) {
            Some(index) => {
                pending.remove(index);
                Err(Injected)
            }
            None => Ok(()),
        }
    }
}

/// Polls two futures concurrently until both finish.
pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let (mut left, mut right) = (None, None);
    poll_fn(|cx| {
        if left.is_none()
            && let Poll::Ready(value) = a.as_mut().poll(cx)
        {
            left = Some(value);
        }
        if right.is_none()
            && let Poll::Ready(value) = b.as_mut().poll(cx)
        {
            right = Some(value);
        }
        if left.is_some() && right.is_some() {
            Poll::Ready((left.take().unwrap(), right.take().unwrap()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// Instantiates every contract case as a test for one store.
///
/// ```ignore
/// hermes_core::contract_tests!(#[tokio::test] () => MemStore::new(installation()));
/// hermes_core::contract_tests!(#[sqlx::test] (pool: PgPool) => fixture(pool).await);
/// ```
#[macro_export]
macro_rules! contract_tests {
    (#[$attr:meta] $params:tt => $store:expr) => {
        $crate::contract_tests!(@cases #[$attr] $params => $store;
            bootstrap_creates_one_manager_and_replays_its_receipt,
            changed_body_with_the_same_key_is_an_idempotency_conflict,
            idempotency_keys_are_scoped_to_the_actor,
            rejected_command_leaves_no_receipt_audit_or_state,
            crash_before_commit_persists_nothing,
            crash_after_commit_replays_the_recorded_receipt,
            concurrent_same_key_requests_apply_once,
            actors_need_a_current_matching_grant,
            last_manager_survives_concurrent_revocations,
            foreign_installation_is_refused_without_writes,
            stale_version_is_a_precondition_failure,
            access_reads_follow_committed_grants,
            drafts_are_versioned_and_publication_validates_settings,
            stale_revisions_and_unknown_versions_are_refused,
            publication_requires_referenced_secrets_of_the_right_kind,
            bob_gets_a_shared_workspace_and_his_network_home,
            concurrent_enables_for_one_identity_commission_one_instance,
            missing_volumes_default_from_the_template,
            only_published_versions_can_be_newly_selected,
            owners_share_static_paths_even_concurrently,
            updates_need_the_current_version_and_advance_generations,
            mount_rules_are_enforced_when_saving,
            secrets_are_sealed_versions_with_a_fixed_kind,
            referenced_secrets_cannot_be_deleted_and_new_versions_restart_users,
            a_secret_delete_racing_a_publish_that_names_it_leaves_no_dangling_reference,
            a_new_instance_converges_to_ready_with_material_created_once,
            a_crash_at_any_controller_point_resumes_without_regenerating_material,
            a_vault_that_opens_no_stored_material_is_refused_before_any_work,
            a_home_the_initializer_refuses_blocks_with_the_typed_reason_until_it_verifies,
            a_deposed_controller_cannot_record_status,
            suspension_stops_then_retains_and_reenabling_reuses_everything,
            a_rejected_apply_blocks_with_a_reason,
            an_invalid_receipt_blocks_and_never_reaches_ready,
            a_missing_static_directory_blocks_until_it_can_be_mounted,
            secrets_are_delivered_opened_at_their_latest_version,
            acquisition_waits_for_a_ready_environment_then_grants_and_replays,
            only_the_assignments_backends_and_mode_are_served,
            leases_renew_release_and_expire_by_database_time_and_never_revive,
            parent_leases_join_only_live_leases_in_the_same_environment,
            a_lease_never_outlives_its_backends_lease_limit,
            closing_a_session_drains_its_environment_and_waits_for_retirement,
            each_instance_holds_a_bounded_number_of_environments,
            workers_start_only_after_ssh_verification_and_drain_when_suspended,
            a_failed_worker_is_reported_and_never_replaced,
            orphans_are_reported_by_the_current_leader_and_never_adopted,
            each_identity_reaches_only_its_own_instance,
            unassigned_and_foreign_identities_are_denied_without_side_effects,
            decisions_follow_access_and_readiness_without_waiting_for_the_controller,
            generated_command_walks_follow_the_model_and_record_only_accepted_commands,
            revocation_racing_acquisitions_grants_nothing_after_it_commits,
            concurrent_heartbeat_and_release_leave_the_lease_finally_dead,
            a_heartbeat_racing_the_expiry_sweep_never_revives_an_expired_lease,
            concurrent_acquisitions_with_one_request_id_grant_one_lease,
        );
    };
    (@cases #[$attr:meta] $params:tt => $store:expr; $($case:ident),+ $(,)?) => {
        $(
            #[$attr]
            async fn $case $params {
                $crate::app::testing::contracts::$case($store).await
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::installation;
    use super::memory::MemStore;

    crate::contract_tests!(#[tokio::test] () => MemStore::new(installation()));
}
