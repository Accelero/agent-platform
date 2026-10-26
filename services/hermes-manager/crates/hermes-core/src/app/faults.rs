//! Named fault points for crash testing.
//!
//! Production wiring uses [`NoFaults`]. Tests inject a hook that fails at a
//! chosen point; the `fault-injection` build of the binaries aborts the process
//! there instead.

/// A boundary at which a crash must not break the recovery guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    EnvelopeBeforeCommit,
    EnvelopeAfterCommit,
    ControllerAfterMaterialInsert,
    ControllerBeforeApply,
    ControllerAfterApply,
    ControllerAfterDelete,
    ControllerBeforeStatus,
}

impl FaultPoint {
    pub const ALL: &[Self] = &[
        Self::EnvelopeBeforeCommit,
        Self::EnvelopeAfterCommit,
        Self::ControllerAfterMaterialInsert,
        Self::ControllerBeforeApply,
        Self::ControllerAfterApply,
        Self::ControllerAfterDelete,
        Self::ControllerBeforeStatus,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::EnvelopeBeforeCommit => "envelope_before_commit",
            Self::EnvelopeAfterCommit => "envelope_after_commit",
            Self::ControllerAfterMaterialInsert => "controller_after_material_insert",
            Self::ControllerBeforeApply => "controller_before_apply",
            Self::ControllerAfterApply => "controller_after_apply",
            Self::ControllerAfterDelete => "controller_after_delete",
            Self::ControllerBeforeStatus => "controller_before_status",
        }
    }
}

/// Called at every fault point. An error simulates the process dying there.
pub trait FaultHook: Send + Sync {
    fn hit(&self, point: FaultPoint) -> Result<(), Injected>;
}

/// The simulated crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Injected;

/// Production hook: never fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFaults;

impl FaultHook for NoFaults {
    fn hit(&self, _: FaultPoint) -> Result<(), Injected> {
        Ok(())
    }
}
