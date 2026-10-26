//! Instance reconciliation: from what PostgreSQL wants and what the cluster
//! shows, the next actions and the status to record. Pure and total: every
//! desired × observed combination has exactly one plan.

use std::collections::BTreeMap;

use serde::Serialize;

use super::assignments::Access;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Enabled; objects being created or updated; not yet ready.
    Provisioning,
    /// Desired generation applied, Deployment available, home initialization
    /// and native authentication gate verified.
    Ready,
    /// Not enabled; Hermes and terminal work stopping.
    Stopping,
    /// Not enabled; nothing running; all data and credentials kept.
    Retained,
    /// Needs an administrator; see [`BlockedReason`].
    Blocked,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Retained => "retained",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "provisioning" => Self::Provisioning,
            "ready" => Self::Ready,
            "stopping" => Self::Stopping,
            "retained" => Self::Retained,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedReason {
    /// The namespace exists without this installation's labels, or with a
    /// UID other than the bound one.
    NamespaceConflict,
    /// The bound namespace is gone. Data in it is gone too; nothing recreates
    /// it under the same binding.
    NamespaceMissing,
    /// A claim lost its volume.
    StorageUnavailable,
    /// The home-initialization receipt does not verify for this owner.
    InvalidInitializationReceipt,
    /// The native authentication gate answered, but not as required.
    NativeAuthInvalid,
    /// The cluster refused the rendered objects, for example by admission.
    ApplyRejected,
}

impl BlockedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NamespaceConflict => "namespace_conflict",
            Self::NamespaceMissing => "namespace_missing",
            Self::StorageUnavailable => "storage_unavailable",
            Self::InvalidInitializationReceipt => "invalid_initialization_receipt",
            Self::NativeAuthInvalid => "native_auth_invalid",
            Self::ApplyRejected => "apply_rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConditionType {
    Storage,
    NativeInit,
    NativeAuth,
}

impl ConditionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::NativeInit => "nativeInit",
            Self::NativeAuth => "nativeAuth",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

impl ConditionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Condition {
    pub kind: ConditionType,
    pub status: ConditionStatus,
    pub reason: &'static str,
}

impl Condition {
    const fn new(kind: ConditionType, status: ConditionStatus, reason: &'static str) -> Self {
        Self {
            kind,
            status,
            reason,
        }
    }
}

/// What the controller records after one reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceStatus {
    pub phase: Phase,
    pub blocked: Option<BlockedReason>,
    pub conditions: Vec<Condition>,
    /// Whether the observed state realizes the desired generation.
    pub converged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimPhase {
    Pending,
    Bound,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedNamespace {
    pub uid: String,
    /// Carries this installation's and this instance's labels.
    pub owned: bool,
    pub terminating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedDeployment {
    pub uid: String,
    pub resource_version: String,
    pub replicas: i32,
    /// The desired generation the controller last applied to it.
    pub applied_generation: Option<i64>,
    /// The rollout of its current spec has completed with every replica
    /// available.
    pub rolled_out: bool,
}

/// The native checks, made only once the Deployment is rolled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCheck {
    NotChecked,
    /// Receipt verified and the auth gate answered as required.
    Verified,
    /// The running Pod reports no receipt yet.
    ReceiptMissing,
    ReceiptInvalid,
    /// The probe could not reach the gate; retried.
    ProbeUnavailable,
    /// The gate answered, but not as required.
    AuthInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub namespace: Option<ObservedNamespace>,
    pub deployment: Option<ObservedDeployment>,
    /// Hermes Pods present, including terminating ones.
    pub hermes_pods: u32,
    /// Terminal backend Pods present, including terminating ones.
    pub worker_pods: u32,
    /// Exactly one Hermes Pod is ready and belongs to the current rollout.
    pub current_pod_ready: bool,
    pub claims: BTreeMap<String, ClaimPhase>,
    pub native: NativeCheck,
    /// A Hermes Pod that is not running reports a failed volume mount, such
    /// as a static directory that does not exist.
    pub mount_failed: bool,
}

impl Observed {
    /// Nothing exists yet.
    pub fn absent() -> Self {
        Self {
            namespace: None,
            deployment: None,
            hermes_pods: 0,
            worker_pods: 0,
            current_pod_ready: false,
            claims: BTreeMap::new(),
            native: NativeCheck::NotChecked,
            mount_failed: false,
        }
    }
}

/// The parts of the desired state the planner decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted<'a> {
    pub access: Access,
    pub generation: i64,
    pub namespace_uid: Option<&'a str>,
    /// Claims the instance needs: the home and every assigned volume.
    pub claims: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// One Hermes replica.
    Run,
    /// Zero replicas.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// The whole object set.
    Everything(Workload),
    /// Only the Deployment's replica count; nothing else changes while the
    /// instance is not enabled.
    Scale(Workload),
    Nothing,
}

/// Delete exactly the observed object and no newer incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Precondition {
    pub uid: String,
    pub resource_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Record this namespace UID; set only when none is bound.
    pub bind_namespace: Option<String>,
    pub apply: Apply,
    pub delete_deployment: Option<Precondition>,
    pub status: InstanceStatus,
    /// Reconcile again after this many seconds; `None` waits for a change or
    /// the periodic resync.
    pub requeue_seconds: Option<u64>,
}

impl Plan {
    /// Whether the controller should run the native checks: the Deployment
    /// has rolled out the current generation with one ready Pod.
    pub fn wants_native_checks(wanted: &Wanted<'_>, observed: &Observed) -> bool {
        wanted.access == Access::Enabled && rolled_out(wanted, observed)
    }
}

fn rolled_out(wanted: &Wanted<'_>, observed: &Observed) -> bool {
    observed.deployment.as_ref().is_some_and(|deployment| {
        deployment.applied_generation == Some(wanted.generation)
            && deployment.rolled_out
            && deployment.replicas == 1
    }) && observed.current_pod_ready
}

fn storage(wanted: &Wanted<'_>, observed: &Observed) -> (Condition, bool) {
    let mut pending = false;
    for claim in &wanted.claims {
        match observed.claims.get(claim) {
            Some(ClaimPhase::Bound) => {}
            Some(ClaimPhase::Lost) => {
                return (
                    Condition::new(ConditionType::Storage, ConditionStatus::False, "lost"),
                    true,
                );
            }
            Some(ClaimPhase::Pending) | None => pending = true,
        }
    }
    // Never replaced by an empty directory: the instance waits, blocked,
    // until the volume can be mounted.
    if observed.mount_failed && !observed.current_pod_ready {
        return (
            Condition::new(
                ConditionType::Storage,
                ConditionStatus::False,
                "mount_failed",
            ),
            true,
        );
    }
    let condition = if pending {
        Condition::new(ConditionType::Storage, ConditionStatus::False, "pending")
    } else {
        Condition::new(ConditionType::Storage, ConditionStatus::True, "bound")
    };
    (condition, false)
}

/// The plan when the cluster refused the objects of `plan`: keep nothing
/// moving, report it and retry later.
pub fn rejected(plan: &Plan) -> Plan {
    let mut result = blocked(BlockedReason::ApplyRejected, plan.status.conditions.clone());
    result.bind_namespace = plan.bind_namespace.clone();
    result
}

fn blocked(reason: BlockedReason, conditions: Vec<Condition>) -> Plan {
    Plan {
        bind_namespace: None,
        apply: Apply::Nothing,
        delete_deployment: None,
        status: InstanceStatus {
            phase: Phase::Blocked,
            blocked: Some(reason),
            conditions,
            converged: false,
        },
        requeue_seconds: Some(60),
    }
}

fn native_conditions(
    init: (ConditionStatus, &'static str),
    auth: (ConditionStatus, &'static str),
) -> [Condition; 2] {
    [
        Condition::new(ConditionType::NativeInit, init.0, init.1),
        Condition::new(ConditionType::NativeAuth, auth.0, auth.1),
    ]
}

/// The next step for one instance.
pub fn plan(wanted: &Wanted<'_>, observed: &Observed) -> Plan {
    // Ownership first: nothing is applied into a namespace that is not ours.
    let bind_namespace = match (&observed.namespace, wanted.namespace_uid) {
        (Some(namespace), _) if !namespace.owned || namespace.terminating => {
            return blocked(BlockedReason::NamespaceConflict, Vec::new());
        }
        (Some(namespace), Some(bound)) if namespace.uid != bound => {
            return blocked(BlockedReason::NamespaceConflict, Vec::new());
        }
        (None, Some(_)) => return blocked(BlockedReason::NamespaceMissing, Vec::new()),
        (Some(namespace), None) => Some(namespace.uid.clone()),
        (Some(_), Some(_)) | (None, None) => None,
    };
    let (storage, lost) = storage(wanted, observed);
    let mut plan = if wanted.access == Access::Enabled {
        enabled(wanted, observed, storage, lost)
    } else {
        stopped(observed, storage)
    };
    plan.bind_namespace = bind_namespace;
    plan
}

fn enabled(wanted: &Wanted<'_>, observed: &Observed, storage: Condition, lost: bool) -> Plan {
    if lost {
        let mut plan = blocked(BlockedReason::StorageUnavailable, vec![storage]);
        // The rest stays applied so a restored volume recovers by itself.
        plan.apply = Apply::Everything(Workload::Run);
        return plan;
    }
    let provisioning = |native: [Condition; 2], requeue: u64| Plan {
        bind_namespace: None,
        apply: Apply::Everything(Workload::Run),
        delete_deployment: None,
        status: InstanceStatus {
            phase: Phase::Provisioning,
            blocked: None,
            conditions: [vec![storage], native.to_vec()].concat(),
            converged: false,
        },
        requeue_seconds: Some(requeue),
    };
    if !rolled_out(wanted, observed) {
        return provisioning(
            native_conditions(
                (ConditionStatus::Unknown, "waiting"),
                (ConditionStatus::Unknown, "waiting"),
            ),
            5,
        );
    }
    match observed.native {
        NativeCheck::Verified => Plan {
            bind_namespace: None,
            apply: Apply::Everything(Workload::Run),
            delete_deployment: None,
            status: InstanceStatus {
                phase: Phase::Ready,
                blocked: None,
                conditions: [
                    vec![storage],
                    native_conditions(
                        (ConditionStatus::True, "verified"),
                        (ConditionStatus::True, "verified"),
                    )
                    .to_vec(),
                ]
                .concat(),
                converged: true,
            },
            requeue_seconds: None,
        },
        NativeCheck::NotChecked | NativeCheck::ReceiptMissing => provisioning(
            native_conditions(
                (ConditionStatus::Unknown, "waiting"),
                (ConditionStatus::Unknown, "waiting"),
            ),
            5,
        ),
        NativeCheck::ProbeUnavailable => provisioning(
            native_conditions(
                (ConditionStatus::True, "verified"),
                (ConditionStatus::Unknown, "unreachable"),
            ),
            5,
        ),
        NativeCheck::ReceiptInvalid => {
            let mut plan = blocked(
                BlockedReason::InvalidInitializationReceipt,
                [
                    vec![storage],
                    native_conditions(
                        (ConditionStatus::False, "invalid_receipt"),
                        (ConditionStatus::Unknown, "waiting"),
                    )
                    .to_vec(),
                ]
                .concat(),
            );
            plan.apply = Apply::Everything(Workload::Run);
            plan
        }
        NativeCheck::AuthInvalid => {
            let mut plan = blocked(
                BlockedReason::NativeAuthInvalid,
                [
                    vec![storage],
                    native_conditions(
                        (ConditionStatus::True, "verified"),
                        (ConditionStatus::False, "invalid_gate"),
                    )
                    .to_vec(),
                ]
                .concat(),
            );
            plan.apply = Apply::Everything(Workload::Run);
            plan
        }
    }
}

fn stopped(observed: &Observed, storage: Condition) -> Plan {
    let native = native_conditions(
        (ConditionStatus::Unknown, "stopped"),
        (ConditionStatus::Unknown, "stopped"),
    );
    let conditions = if observed.namespace.is_some() {
        [vec![storage], native.to_vec()].concat()
    } else {
        native.to_vec()
    };
    let stopping = |apply: Apply, delete: Option<Precondition>| Plan {
        bind_namespace: None,
        apply,
        delete_deployment: delete,
        status: InstanceStatus {
            phase: Phase::Stopping,
            blocked: None,
            conditions: conditions.clone(),
            converged: false,
        },
        requeue_seconds: Some(2),
    };
    match &observed.deployment {
        Some(deployment) if deployment.replicas > 0 || observed.hermes_pods > 0 => {
            stopping(Apply::Scale(Workload::Stop), None)
        }
        Some(_) if observed.worker_pods > 0 => stopping(Apply::Nothing, None),
        Some(deployment) => stopping(
            Apply::Nothing,
            Some(Precondition {
                uid: deployment.uid.clone(),
                resource_version: deployment.resource_version.clone(),
            }),
        ),
        None if observed.hermes_pods > 0 || observed.worker_pods > 0 => {
            stopping(Apply::Nothing, None)
        }
        None => Plan {
            bind_namespace: None,
            apply: Apply::Nothing,
            delete_deployment: None,
            status: InstanceStatus {
                phase: Phase::Retained,
                blocked: None,
                conditions,
                converged: true,
            },
            requeue_seconds: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "0f9e8d7c-6b5a-4938-8271-605f4e3d2c1b";

    fn wanted(access: Access) -> Wanted<'static> {
        Wanted {
            access,
            generation: 3,
            namespace_uid: Some(UID),
            claims: vec!["hermes-home".into(), "vol-workspace".into()],
        }
    }

    fn namespace() -> ObservedNamespace {
        ObservedNamespace {
            uid: UID.into(),
            owned: true,
            terminating: false,
        }
    }

    fn deployment(replicas: i32, applied: i64, rolled_out: bool) -> ObservedDeployment {
        ObservedDeployment {
            uid: "d-uid".into(),
            resource_version: "41".into(),
            replicas,
            applied_generation: Some(applied),
            rolled_out,
        }
    }

    fn running(native: NativeCheck) -> Observed {
        Observed {
            namespace: Some(namespace()),
            deployment: Some(deployment(1, 3, true)),
            hermes_pods: 1,
            worker_pods: 0,
            current_pod_ready: true,
            claims: BTreeMap::from([
                ("hermes-home".into(), ClaimPhase::Bound),
                ("vol-workspace".into(), ClaimPhase::Bound),
            ]),
            native,
            mount_failed: false,
        }
    }

    fn condition(plan: &Plan, kind: ConditionType) -> (ConditionStatus, &'static str) {
        let found = plan
            .status
            .conditions
            .iter()
            .find(|c| c.kind == kind)
            .expect("condition");
        (found.status, found.reason)
    }

    #[test]
    fn a_new_instance_creates_everything_and_binds_its_namespace_on_first_sight() {
        let mut fresh = wanted(Access::Enabled);
        fresh.namespace_uid = None;
        let first = plan(&fresh, &Observed::absent());
        assert_eq!(first.apply, Apply::Everything(Workload::Run));
        assert_eq!(first.bind_namespace, None);
        assert_eq!(first.status.phase, Phase::Provisioning);
        assert!(!first.status.converged);

        let observed = Observed {
            namespace: Some(namespace()),
            ..Observed::absent()
        };
        let second = plan(&fresh, &observed);
        assert_eq!(second.bind_namespace.as_deref(), Some(UID));
        assert_eq!(
            condition(&second, ConditionType::Storage),
            (ConditionStatus::False, "pending")
        );
    }

    #[test]
    fn a_failed_volume_mount_blocks_with_a_storage_condition_and_keeps_applying() {
        let mut starting = running(NativeCheck::NotChecked);
        starting.current_pod_ready = false;
        starting.deployment = Some(deployment(1, 3, false));
        starting.mount_failed = true;
        let blocked = plan(&wanted(Access::Enabled), &starting);
        assert_eq!(blocked.status.phase, Phase::Blocked);
        assert_eq!(
            blocked.status.blocked,
            Some(BlockedReason::StorageUnavailable)
        );
        assert_eq!(
            condition(&blocked, ConditionType::Storage),
            (ConditionStatus::False, "mount_failed")
        );
        // Everything stays applied, so the instance starts once the
        // directory exists.
        assert_eq!(blocked.apply, Apply::Everything(Workload::Run));
        starting.mount_failed = false;
        assert_eq!(
            plan(&wanted(Access::Enabled), &starting).status.phase,
            Phase::Provisioning
        );
    }

    #[test]
    fn ready_needs_the_current_rollout_a_verified_receipt_and_the_auth_gate() {
        let ready = plan(&wanted(Access::Enabled), &running(NativeCheck::Verified));
        assert_eq!(ready.status.phase, Phase::Ready);
        assert!(ready.status.converged);
        assert_eq!(ready.requeue_seconds, None);
        assert_eq!(ready.apply, Apply::Everything(Workload::Run));

        let mut older = running(NativeCheck::Verified);
        older.deployment = Some(deployment(1, 2, true));
        assert_eq!(
            plan(&wanted(Access::Enabled), &older).status.phase,
            Phase::Provisioning
        );
        assert!(!Plan::wants_native_checks(&wanted(Access::Enabled), &older));

        let mut rolling = running(NativeCheck::NotChecked);
        rolling.deployment = Some(deployment(1, 3, false));
        assert!(!Plan::wants_native_checks(
            &wanted(Access::Enabled),
            &rolling
        ));
        assert_eq!(
            plan(&wanted(Access::Enabled), &rolling).status.phase,
            Phase::Provisioning
        );

        let mut two_pods = running(NativeCheck::Verified);
        two_pods.current_pod_ready = false;
        assert_eq!(
            plan(&wanted(Access::Enabled), &two_pods).status.phase,
            Phase::Provisioning
        );
        assert!(Plan::wants_native_checks(
            &wanted(Access::Enabled),
            &running(NativeCheck::NotChecked)
        ));
    }

    #[test]
    fn native_failures_are_typed_and_only_definite_ones_block() {
        let cases = [
            (NativeCheck::NotChecked, Phase::Provisioning, None),
            (NativeCheck::ReceiptMissing, Phase::Provisioning, None),
            (NativeCheck::ProbeUnavailable, Phase::Provisioning, None),
            (
                NativeCheck::ReceiptInvalid,
                Phase::Blocked,
                Some(BlockedReason::InvalidInitializationReceipt),
            ),
            (
                NativeCheck::AuthInvalid,
                Phase::Blocked,
                Some(BlockedReason::NativeAuthInvalid),
            ),
        ];
        for (native, phase, reason) in cases {
            let result = plan(&wanted(Access::Enabled), &running(native));
            assert_eq!(
                (result.status.phase, result.status.blocked),
                (phase, reason),
                "{native:?}"
            );
            assert_eq!(result.apply, Apply::Everything(Workload::Run), "{native:?}");
            assert!(result.requeue_seconds.is_some());
        }
        let invalid = plan(
            &wanted(Access::Enabled),
            &running(NativeCheck::ReceiptInvalid),
        );
        assert_eq!(
            condition(&invalid, ConditionType::NativeInit),
            (ConditionStatus::False, "invalid_receipt")
        );
    }

    #[test]
    fn foreign_recreated_or_missing_namespaces_block_without_any_write() {
        let mut foreign = running(NativeCheck::Verified);
        foreign.namespace.as_mut().unwrap().owned = false;
        let mut recreated = running(NativeCheck::Verified);
        recreated.namespace.as_mut().unwrap().uid = "another-uid".into();
        let mut terminating = running(NativeCheck::Verified);
        terminating.namespace.as_mut().unwrap().terminating = true;
        for (observed, reason) in [
            (foreign, BlockedReason::NamespaceConflict),
            (recreated, BlockedReason::NamespaceConflict),
            (terminating, BlockedReason::NamespaceConflict),
            (Observed::absent(), BlockedReason::NamespaceMissing),
        ] {
            for access in [Access::Enabled, Access::Suspended, Access::Revoked] {
                let result = plan(&wanted(access), &observed);
                assert_eq!(result.status.blocked, Some(reason));
                assert_eq!(result.apply, Apply::Nothing);
                assert_eq!(result.delete_deployment, None);
                assert_eq!(result.bind_namespace, None);
            }
        }
        let mut unbound = wanted(Access::Enabled);
        unbound.namespace_uid = None;
        let mut foreign = Observed::absent();
        foreign.namespace = Some(ObservedNamespace {
            uid: "x".into(),
            owned: false,
            terminating: false,
        });
        assert_eq!(
            plan(&unbound, &foreign).bind_namespace,
            None,
            "never bind a foreign namespace"
        );
    }

    #[test]
    fn a_lost_volume_blocks_but_keeps_the_objects_applied() {
        let mut observed = running(NativeCheck::Verified);
        observed
            .claims
            .insert("vol-workspace".into(), ClaimPhase::Lost);
        let result = plan(&wanted(Access::Enabled), &observed);
        assert_eq!(
            result.status.blocked,
            Some(BlockedReason::StorageUnavailable)
        );
        assert_eq!(result.apply, Apply::Everything(Workload::Run));
        let mut missing = running(NativeCheck::Verified);
        missing.claims.remove("vol-workspace");
        let result = plan(&wanted(Access::Enabled), &missing);
        assert_eq!(
            result.status.phase,
            Phase::Ready,
            "claims are checked; Pods already run on them"
        );
        assert_eq!(
            condition(&result, ConditionType::Storage),
            (ConditionStatus::False, "pending")
        );
    }

    #[test]
    fn stopping_scales_down_waits_for_pods_and_workers_then_deletes_exactly_what_it_saw() {
        for access in [Access::Suspended, Access::Revoked] {
            let first = plan(&wanted(access), &running(NativeCheck::NotChecked));
            assert_eq!(first.apply, Apply::Scale(Workload::Stop));
            assert_eq!(first.status.phase, Phase::Stopping);

            let mut scaled = running(NativeCheck::NotChecked);
            scaled.deployment = Some(deployment(0, 3, true));
            scaled.hermes_pods = 1;
            assert_eq!(
                plan(&wanted(access), &scaled).apply,
                Apply::Scale(Workload::Stop)
            );

            scaled.hermes_pods = 0;
            scaled.worker_pods = 2;
            let waiting = plan(&wanted(access), &scaled);
            assert_eq!(
                (waiting.apply, waiting.delete_deployment.as_ref()),
                (Apply::Nothing, None)
            );

            scaled.worker_pods = 0;
            let delete = plan(&wanted(access), &scaled);
            assert_eq!(
                delete.delete_deployment,
                Some(Precondition {
                    uid: "d-uid".into(),
                    resource_version: "41".into()
                })
            );
            assert_eq!(delete.status.phase, Phase::Stopping);
            assert!(
                !delete.status.converged,
                "completion needs a later observation of absence"
            );

            scaled.deployment = None;
            let retained = plan(&wanted(access), &scaled);
            assert_eq!(retained.status.phase, Phase::Retained);
            assert!(retained.status.converged);
            assert_eq!(retained.apply, Apply::Nothing);
        }
    }

    #[test]
    fn an_instance_never_started_is_retained_without_creating_anything() {
        let mut never = wanted(Access::Suspended);
        never.namespace_uid = None;
        let result = plan(&never, &Observed::absent());
        assert_eq!(result.status.phase, Phase::Retained);
        assert_eq!(result.apply, Apply::Nothing);
        assert!(
            result
                .status
                .conditions
                .iter()
                .all(|c| c.kind != ConditionType::Storage)
        );
    }

    #[test]
    fn re_enabling_restarts_the_same_objects() {
        let mut stopped = running(NativeCheck::NotChecked);
        stopped.deployment = None;
        stopped.hermes_pods = 0;
        let result = plan(&wanted(Access::Enabled), &stopped);
        assert_eq!(result.apply, Apply::Everything(Workload::Run));
        assert_eq!(result.status.phase, Phase::Provisioning);
        assert_eq!(result.bind_namespace, None, "the binding is kept");
    }

    /// Every combination has a plan, and none deletes while enabled or
    /// applies into a namespace it does not own.
    #[test]
    fn the_planner_is_total_and_never_contradicts_itself() {
        let namespaces = [
            None,
            Some(namespace()),
            Some(ObservedNamespace {
                uid: "other".into(),
                owned: true,
                terminating: false,
            }),
            Some(ObservedNamespace {
                uid: UID.into(),
                owned: false,
                terminating: false,
            }),
        ];
        let deployments = [
            None,
            Some(deployment(1, 3, true)),
            Some(deployment(1, 2, true)),
            Some(deployment(0, 3, true)),
            Some(deployment(1, 3, false)),
        ];
        let natives = [
            NativeCheck::NotChecked,
            NativeCheck::Verified,
            NativeCheck::ReceiptMissing,
            NativeCheck::ReceiptInvalid,
            NativeCheck::ProbeUnavailable,
            NativeCheck::AuthInvalid,
        ];
        let claims = [ClaimPhase::Pending, ClaimPhase::Bound, ClaimPhase::Lost];
        for access in [Access::Enabled, Access::Suspended, Access::Revoked] {
            for bound in [None, Some(UID)] {
                for ns in &namespaces {
                    for d in &deployments {
                        for pods in [0, 1, 2] {
                            for workers in [0, 1] {
                                for ready in [false, true] {
                                    for native in natives {
                                        for claim in claims {
                                            let wanted = Wanted {
                                                access,
                                                generation: 3,
                                                namespace_uid: bound,
                                                claims: vec!["hermes-home".into()],
                                            };
                                            let observed = Observed {
                                                namespace: ns.clone(),
                                                deployment: d.clone(),
                                                hermes_pods: pods,
                                                worker_pods: workers,
                                                current_pod_ready: ready,
                                                claims: BTreeMap::from([(
                                                    "hermes-home".into(),
                                                    claim,
                                                )]),
                                                native,
                                                mount_failed: !ready && pods == 1,
                                            };
                                            let result = plan(&wanted, &observed);
                                            let owned = ns.as_ref().is_none_or(|n| {
                                                n.owned && bound.is_none_or(|b| b == n.uid)
                                            });
                                            if !owned || (ns.is_none() && bound.is_some()) {
                                                assert_eq!(result.apply, Apply::Nothing);
                                                assert_eq!(result.delete_deployment, None);
                                            }
                                            if access == Access::Enabled {
                                                assert_eq!(result.delete_deployment, None);
                                                assert!(!matches!(result.apply, Apply::Scale(_)));
                                            } else {
                                                assert!(!matches!(
                                                    result.apply,
                                                    Apply::Everything(_)
                                                ));
                                            }
                                            if result.status.converged {
                                                assert!(matches!(
                                                    result.status.phase,
                                                    Phase::Ready | Phase::Retained
                                                ));
                                            }
                                            assert_eq!(
                                                result.status.blocked.is_some(),
                                                result.status.phase == Phase::Blocked
                                            );
                                            if result.delete_deployment.is_some() {
                                                assert_eq!(
                                                    observed.hermes_pods + observed.worker_pods,
                                                    0
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
