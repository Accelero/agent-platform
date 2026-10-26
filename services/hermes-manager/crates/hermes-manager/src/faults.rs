//! The process fault hook. With the `fault-injection` feature, the process
//! aborts at the point named by `HERMES_FAULT_POINT`, simulating a crash with
//! no shutdown hooks. Without it, faults never fire.

use std::sync::Arc;

use hermes_core::app::faults::{FaultHook, NoFaults};

#[cfg(feature = "fault-injection")]
struct Abort(String);

#[cfg(feature = "fault-injection")]
impl FaultHook for Abort {
    fn hit(
        &self,
        point: hermes_core::app::faults::FaultPoint,
    ) -> Result<(), hermes_core::app::faults::Injected> {
        if point.name() == self.0 {
            std::process::abort();
        }
        Ok(())
    }
}

pub fn hook() -> Arc<dyn FaultHook> {
    #[cfg(feature = "fault-injection")]
    if let Ok(point) = std::env::var("HERMES_FAULT_POINT") {
        return Arc::new(Abort(point));
    }
    Arc::new(NoFaults)
}
