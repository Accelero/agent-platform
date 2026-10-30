//! Terminal execution: environments, leases and the environment planner.
//!
//! An environment is one worker Pod for one instance, backend version,
//! execution generation and key (the session, or the profile). Its state
//! only moves forward:
//!
//! ```text
//! Pending ─Pod applied─▶ Starting ─Pod ready, SSH verified─▶ Ready
//!    └──────────── drain trigger ──────────────────────────────┘
//!                          ▼
//!                      Draining ─Pod absent (exact UID)─▶ Retired
//! ```
//!
//! Leases are `Active` until released or expired; both are final.

use serde::{Deserialize, Serialize};

use super::assignments::{Access, LifetimeMode, TerminalGrant};
use super::instances::Precondition;
use super::names::validate_name;
use super::{ErrorKind, Rejection};

/// Lease lifetime granted per acquire or heartbeat, in seconds.
pub const LEASE_TTL_SECONDS: u32 = 120;
/// How long live leases hold off a drain.
pub const DRAIN_DEADLINE_SECONDS: u32 = 300;
/// Environments one instance may hold across all backends.
pub const MAX_INSTANCE_ENVIRONMENTS: u32 = 64;
/// The worker's SSH port and user (`managed-ssh-v2`).
pub const SSH_PORT: u16 = 2222;
pub const SSH_USER: &str = "worker";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentState {
    Pending,
    Starting,
    Ready,
    Draining,
    Retired,
}

impl EnvironmentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::Retired => "retired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "starting" => Self::Starting,
            "ready" => Self::Ready,
            "draining" => Self::Draining,
            "retired" => Self::Retired,
            _ => return None,
        })
    }

    /// Whether `next` may follow this state.
    pub fn allows(self, next: Self) -> bool {
        use EnvironmentState::*;
        matches!(
            (self, next),
            (Pending, Starting)
                | (Starting, Ready)
                | (Pending | Starting | Ready, Draining)
                | (Draining, Retired)
        )
    }

    /// Stale environments get no new leases.
    pub fn stale(self) -> bool {
        matches!(self, Self::Draining | Self::Retired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Active,
    Released,
    Expired,
}

impl LeaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Released => "released",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionRejected {
    /// The credential is unknown or its instance is not enabled.
    Unauthenticated,
    /// The backend is not allowed, the mode differs, or the lease is not this
    /// instance's.
    Denied,
    /// The environment is not ready yet; retry with the same request.
    Pending,
    /// The request identifier was used with a different body.
    IdempotencyConflict,
    /// The lease is released, expired or its environment stale.
    LeaseNotLive,
    NotFound,
    /// The instance holds the maximum number of environments.
    Exhausted,
    /// The backend's worker failed to start or breaks the contract; no other
    /// image is substituted.
    BackendFailed,
}

impl Rejection for ExecutionRejected {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Unauthenticated => ErrorKind::Unauthenticated,
            Self::Denied => ErrorKind::Forbidden,
            Self::Pending => ErrorKind::Pending,
            Self::IdempotencyConflict | Self::LeaseNotLive => ErrorKind::Conflict,
            Self::NotFound => ErrorKind::NotFound,
            Self::Exhausted => ErrorKind::Unavailable,
            Self::BackendFailed => ErrorKind::Invalid,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "invalid_identity_evidence",
            Self::Denied => "access_denied",
            Self::Pending => "environment_pending",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::LeaseNotLive => "lease_not_live",
            Self::NotFound => "not_found",
            Self::Exhausted => "dependency_unavailable",
            Self::BackendFailed => "backend_failed",
        }
    }
}

/// `Identifier` of the execution contract: 1–256 characters, starting
/// alphanumeric, then alphanumerics and `_ . : -`.
pub fn valid_identifier(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// Invocation metadata from the plugin: absolute paths, never mounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub hermes_home: String,
    pub workdir: String,
}

impl Invocation {
    pub fn valid(&self) -> bool {
        [&self.hermes_home, &self.workdir].iter().all(|p| {
            (1..=4096).contains(&p.len()) && p.starts_with('/') && !p.chars().any(char::is_control)
        })
    }
}

/// The backend version an acquisition may use: the requested one if the
/// assignment allows it, else the default. No fallback to anything else.
pub fn resolve_backend(
    grant: &TerminalGrant,
    requested: Option<&str>,
) -> Result<(String, i64), ExecutionRejected> {
    let id = match requested {
        Some(id) => {
            validate_name(id).map_err(|_| ExecutionRejected::Denied)?;
            id
        }
        None => grant.default.as_deref().ok_or(ExecutionRejected::Denied)?,
    };
    grant
        .allowed
        .get(id)
        .map(|version| (id.to_owned(), *version))
        .ok_or(ExecutionRejected::Denied)
}

/// What one environment is keyed by besides instance, backend and generation:
/// the session for `per_session`, the profile's home otherwise.
pub fn environment_key(
    mode: LifetimeMode,
    session: &str,
    invocation: Option<&Invocation>,
) -> String {
    match mode {
        LifetimeMode::PerSession => format!("session:{session}"),
        LifetimeMode::PerProfileOnDemand | LifetimeMode::PerProfileAlwaysRunning => {
            format!(
                "profile:{}",
                invocation.map_or("", |i| i.hermes_home.as_str())
            )
        }
    }
}

/// The session column of an environment summary: empty for profile modes.
pub fn summary_session(key: &str) -> &str {
    key.strip_prefix("session:").unwrap_or("")
}

/// One environment as the controller sees it in PostgreSQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentView {
    pub state: EnvironmentState,
    pub mode: LifetimeMode,
    pub backend: (String, i64),
    pub execution_generation: i64,
    pub pod_uid: Option<String>,
    pub live_leases: u32,
    /// Seconds since the last lease was granted or renewed.
    pub idle_seconds: u64,
    /// Whether a drain deadline is set and has passed.
    pub drain_expired: bool,
}

/// What the controller needs about the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentContext<'a> {
    pub access: Access,
    pub execution_generation: i64,
    pub grant: &'a TerminalGrant,
    /// The backend version's idle timeout.
    pub idle_seconds: u64,
}

/// Why a worker cannot serve its environment. Reported; the environment
/// drains and no other image is ever substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerFailure {
    /// The image cannot be pulled, or the reference is invalid.
    ImageUnavailable,
    /// The container cannot be created, exits or keeps crashing.
    StartFailed,
    /// The worker breaks `managed-ssh-v2`: another host key, the client key
    /// refused, or a shell that does not run as the backend-declared UID.
    ContractFailed,
}

impl WorkerFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageUnavailable => "image_unavailable",
            Self::StartFailed => "start_failed",
            Self::ContractFailed => "contract_failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "image_unavailable" => Self::ImageUnavailable,
            "start_failed" => Self::StartFailed,
            "contract_failed" => Self::ContractFailed,
            _ => return None,
        })
    }
}

/// One SSH readiness check of a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshCheck {
    /// The client key logs in, the pinned host key matches and the shell runs
    /// as the backend-declared UID.
    Ready,
    /// Not reachable or not answering yet; checked again.
    NotYet,
    /// The worker answered but breaks the contract.
    ContractFailed,
}

/// The worker Pod, as observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedWorker {
    pub uid: String,
    pub resource_version: String,
    /// Ready, not terminating, with an address.
    pub ready: Option<String>,
    /// Terminating, failed or succeeded.
    pub gone: bool,
    /// A definite start failure reported by the Pod's status.
    pub failure: Option<WorkerFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStep {
    Nothing,
    /// Apply the worker Pod (and its keys).
    Apply,
    /// Verify SSH at this address before recording `Ready`.
    Verify(String),
    /// Delete exactly this Pod.
    Delete(Precondition),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPlan {
    pub step: WorkerStep,
    /// Move to this state; for `Starting`, record the Pod UID.
    pub transition: Option<EnvironmentState>,
    /// With a transition to `Draining`: why the worker failed.
    pub failure: Option<WorkerFailure>,
    pub requeue_seconds: Option<u64>,
}

fn plan(
    step: WorkerStep,
    transition: Option<EnvironmentState>,
    requeue: Option<u64>,
) -> EnvironmentPlan {
    EnvironmentPlan {
        step,
        transition,
        failure: None,
        requeue_seconds: requeue,
    }
}

/// Why a live environment must drain now, if it must.
pub fn drain_trigger(
    view: &EnvironmentView,
    context: &EnvironmentContext<'_>,
    observed: Option<&ObservedWorker>,
) -> bool {
    let unused = view.live_leases == 0;
    let allowed = context.grant.allowed.get(&view.backend.0) == Some(&view.backend.1);
    let idle = unused
        && view.idle_seconds >= context.idle_seconds
        && matches!(
            view.mode,
            LifetimeMode::PerSession | LifetimeMode::PerProfileOnDemand
        );
    let lost = match (&view.pod_uid, observed) {
        (Some(_), None) => true,
        (Some(uid), Some(pod)) => pod.uid != *uid || pod.gone,
        (None, Some(pod)) => pod.gone,
        (None, None) => false,
    };
    context.access != Access::Enabled
        || (unused && (view.execution_generation < context.execution_generation || !allowed))
        || idle
        || lost
}

/// The next step for one environment. Pure and total.
pub fn plan_environment(
    view: &EnvironmentView,
    context: &EnvironmentContext<'_>,
    observed: Option<&ObservedWorker>,
) -> EnvironmentPlan {
    use EnvironmentState::*;
    if matches!(view.state, Pending | Starting)
        && let Some(failure) = observed.and_then(|pod| pod.failure)
    {
        return EnvironmentPlan {
            failure: Some(failure),
            ..plan(WorkerStep::Nothing, Some(Draining), Some(1))
        };
    }
    if matches!(view.state, Pending | Starting | Ready) && drain_trigger(view, context, observed) {
        return plan(WorkerStep::Nothing, Some(Draining), Some(1));
    }
    match view.state {
        Retired => plan(WorkerStep::Nothing, None, None),
        Pending => match observed {
            // A Pod of this environment already exists: it was applied
            // before a crash; record it instead of applying again.
            Some(pod) if !pod.gone => plan(WorkerStep::Nothing, Some(Starting), Some(1)),
            _ => plan(WorkerStep::Apply, None, Some(2)),
        },
        Starting => match observed.and_then(|pod| pod.ready.clone()) {
            Some(address) => plan(WorkerStep::Verify(address), None, Some(2)),
            None => plan(WorkerStep::Nothing, None, Some(2)),
        },
        Ready => plan(WorkerStep::Nothing, None, Some(30)),
        Draining => match observed {
            None => plan(WorkerStep::Nothing, Some(Retired), None),
            Some(_) if view.live_leases > 0 && !view.drain_expired => {
                plan(WorkerStep::Nothing, None, Some(5))
            }
            Some(pod) => plan(
                WorkerStep::Delete(Precondition {
                    uid: pod.uid.clone(),
                    resource_version: pod.resource_version.clone(),
                }),
                None,
                Some(2),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn grant() -> TerminalGrant {
        TerminalGrant {
            mode: LifetimeMode::PerSession,
            default: Some("python-tools".into()),
            allowed: BTreeMap::from([("python-tools".into(), 2), ("gpu-tools".into(), 1)]),
        }
    }

    fn view(state: EnvironmentState) -> EnvironmentView {
        EnvironmentView {
            state,
            mode: LifetimeMode::PerSession,
            backend: ("python-tools".into(), 2),
            execution_generation: 3,
            pod_uid: (state != EnvironmentState::Pending).then(|| "pod-uid".into()),
            live_leases: 0,
            idle_seconds: 0,
            drain_expired: false,
        }
    }

    fn pod(ready: bool) -> ObservedWorker {
        ObservedWorker {
            uid: "pod-uid".into(),
            resource_version: "7".into(),
            ready: ready.then(|| "10.42.0.9".into()),
            gone: false,
            failure: None,
        }
    }

    #[test]
    fn a_failing_worker_drains_with_its_reason_and_is_never_replaced() {
        let grant = grant();
        for failure in [WorkerFailure::ImageUnavailable, WorkerFailure::StartFailed] {
            let failed = ObservedWorker {
                failure: Some(failure),
                gone: failure == WorkerFailure::StartFailed,
                ..pod(false)
            };
            for state in [EnvironmentState::Pending, EnvironmentState::Starting] {
                let next = plan_environment(&view(state), &context(&grant), Some(&failed));
                assert_eq!(next.step, WorkerStep::Nothing, "{state:?}");
                assert_eq!(next.transition, Some(EnvironmentState::Draining));
                assert_eq!(next.failure, Some(failure));
            }
        }
        // A Pod that only waits for its image to start is not a failure.
        let waiting = plan_environment(
            &view(EnvironmentState::Starting),
            &context(&grant),
            Some(&pod(false)),
        );
        assert_eq!((waiting.transition, waiting.failure), (None, None));
        for failure in ["image_unavailable", "start_failed", "contract_failed"] {
            assert_eq!(
                WorkerFailure::parse(failure).map(WorkerFailure::as_str),
                Some(failure)
            );
        }
    }

    fn context(grant: &TerminalGrant) -> EnvironmentContext<'_> {
        EnvironmentContext {
            access: Access::Enabled,
            execution_generation: 3,
            grant,
            idle_seconds: 900,
        }
    }

    #[test]
    fn states_only_move_forward_and_retired_is_final() {
        use EnvironmentState::*;
        let all = [Pending, Starting, Ready, Draining, Retired];
        for from in all {
            for to in all {
                let expected = matches!(
                    (from, to),
                    (Pending, Starting)
                        | (Starting, Ready)
                        | (Pending | Starting | Ready, Draining)
                        | (Draining, Retired)
                );
                assert_eq!(from.allows(to), expected, "{from:?} -> {to:?}");
            }
            assert_eq!(EnvironmentState::parse(from.as_str()), Some(from));
        }
        assert!(all.iter().all(|s| !Retired.allows(*s)));
    }

    #[test]
    fn backends_resolve_only_within_the_assignment() {
        let grant = grant();
        assert_eq!(
            resolve_backend(&grant, None),
            Ok(("python-tools".into(), 2))
        );
        assert_eq!(
            resolve_backend(&grant, Some("gpu-tools")),
            Ok(("gpu-tools".into(), 1))
        );
        assert_eq!(
            resolve_backend(&grant, Some("docker")),
            Err(ExecutionRejected::Denied)
        );
        assert_eq!(
            resolve_backend(&grant, Some("../x")),
            Err(ExecutionRejected::Denied)
        );
        let mut no_default = grant;
        no_default.default = None;
        assert_eq!(
            resolve_backend(&no_default, None),
            Err(ExecutionRejected::Denied)
        );
    }

    #[test]
    fn keys_separate_sessions_and_profiles() {
        let home = Invocation {
            hermes_home: "/opt/data/profiles/a".into(),
            workdir: "/workspace".into(),
        };
        assert_eq!(
            environment_key(LifetimeMode::PerSession, "s1", Some(&home)),
            "session:s1"
        );
        assert_eq!(
            environment_key(LifetimeMode::PerProfileOnDemand, "s1", Some(&home)),
            environment_key(LifetimeMode::PerProfileOnDemand, "s2", Some(&home)),
            "profile environments are shared across sessions"
        );
        assert_ne!(
            environment_key(LifetimeMode::PerProfileOnDemand, "s1", Some(&home)),
            environment_key(LifetimeMode::PerProfileOnDemand, "s1", None)
        );
        assert_eq!(summary_session("session:s1"), "s1");
        assert_eq!(summary_session("profile:/opt/data"), "");
        assert!(
            valid_identifier("a.b:c-d_e")
                && !valid_identifier("-a")
                && !valid_identifier(&"a".repeat(257))
        );
        assert!(
            !Invocation {
                hermes_home: "relative".into(),
                workdir: "/w".into()
            }
            .valid()
        );
    }

    #[test]
    fn an_environment_is_applied_verified_and_then_ready() {
        let grant = grant();
        let context = context(&grant);
        let pending = view(EnvironmentState::Pending);
        assert_eq!(
            plan_environment(&pending, &context, None).step,
            WorkerStep::Apply
        );
        let found = plan_environment(&pending, &context, Some(&pod(false)));
        assert_eq!(
            found.transition,
            Some(EnvironmentState::Starting),
            "an applied Pod is recorded, not re-applied"
        );
        let starting = view(EnvironmentState::Starting);
        assert_eq!(
            plan_environment(&starting, &context, Some(&pod(false))).step,
            WorkerStep::Nothing
        );
        assert_eq!(
            plan_environment(&starting, &context, Some(&pod(true))).step,
            WorkerStep::Verify("10.42.0.9".into())
        );
        assert_eq!(
            plan_environment(&view(EnvironmentState::Ready), &context, Some(&pod(true))).transition,
            None
        );
    }

    #[test]
    fn drain_triggers_follow_the_specification() {
        let grant = grant();
        let base = context(&grant);
        let ready = view(EnvironmentState::Ready);
        let drains = |view: &EnvironmentView,
                      context: &EnvironmentContext<'_>,
                      observed: Option<&ObservedWorker>| {
            plan_environment(view, context, observed).transition == Some(EnvironmentState::Draining)
        };
        let mut suspended = base.clone();
        suspended.access = Access::Suspended;
        assert!(drains(&ready, &suspended, Some(&pod(true))));

        let mut superseded = base.clone();
        superseded.execution_generation = 4;
        assert!(drains(&ready, &superseded, Some(&pod(true))));
        let mut busy = ready.clone();
        busy.live_leases = 1;
        assert!(
            !drains(&busy, &superseded, Some(&pod(true))),
            "live leases finish first"
        );

        let mut idle = ready.clone();
        idle.idle_seconds = 900;
        assert!(drains(&idle, &base, Some(&pod(true))));
        let mut always = idle.clone();
        always.mode = LifetimeMode::PerProfileAlwaysRunning;
        assert!(
            !drains(&always, &base, Some(&pod(true))),
            "always-running environments never idle out"
        );

        assert!(drains(&ready, &base, None), "a lost Pod drains");
        let mut replaced = pod(true);
        replaced.uid = "other".into();
        assert!(drains(&ready, &base, Some(&replaced)));
        let mut terminating = pod(true);
        terminating.gone = true;
        assert!(drains(&ready, &base, Some(&terminating)));

        let mut withdrawn = grant.clone();
        withdrawn.allowed.remove("python-tools");
        let mut no_backend = base.clone();
        no_backend.grant = &withdrawn;
        assert!(drains(&ready, &no_backend, Some(&pod(true))));
        assert!(!drains(&ready, &base, Some(&pod(true))));
    }

    #[test]
    fn draining_waits_for_leases_until_the_deadline_then_deletes_exactly_the_pod() {
        let grant = grant();
        let context = context(&grant);
        let mut draining = view(EnvironmentState::Draining);
        draining.live_leases = 1;
        assert_eq!(
            plan_environment(&draining, &context, Some(&pod(true))).step,
            WorkerStep::Nothing
        );
        draining.drain_expired = true;
        assert_eq!(
            plan_environment(&draining, &context, Some(&pod(true))).step,
            WorkerStep::Delete(Precondition {
                uid: "pod-uid".into(),
                resource_version: "7".into()
            })
        );
        draining.live_leases = 0;
        draining.drain_expired = false;
        assert!(matches!(
            plan_environment(&draining, &context, Some(&pod(true))).step,
            WorkerStep::Delete(_)
        ));
        let gone = plan_environment(&draining, &context, None);
        assert_eq!(
            gone.transition,
            Some(EnvironmentState::Retired),
            "retired once absence is observed"
        );
        let retired = view(EnvironmentState::Retired);
        assert_eq!(
            plan_environment(&retired, &context, Some(&pod(true))),
            plan(WorkerStep::Nothing, None, None)
        );
    }

    #[test]
    fn the_planner_never_applies_a_stale_environment() {
        use EnvironmentState::*;
        let grant = grant();
        for state in [Pending, Starting, Ready, Draining, Retired] {
            for access in [Access::Enabled, Access::Suspended, Access::Revoked] {
                for generation in [3, 4] {
                    for leases in [0, 2] {
                        for observed in [None, Some(pod(false)), Some(pod(true))] {
                            let mut view = view(state);
                            view.live_leases = leases;
                            let context = EnvironmentContext {
                                access,
                                execution_generation: generation,
                                grant: &grant,
                                idle_seconds: 900,
                            };
                            let result = plan_environment(&view, &context, observed.as_ref());
                            if let Some(next) = result.transition {
                                assert!(state.allows(next), "{state:?} -> {next:?}");
                            }
                            if result.step == WorkerStep::Apply {
                                assert_eq!(state, Pending);
                                assert_eq!(access, Access::Enabled);
                            }
                            if matches!(result.step, WorkerStep::Delete(_)) {
                                assert_eq!(state, Draining);
                            }
                        }
                    }
                }
            }
        }
    }
}
