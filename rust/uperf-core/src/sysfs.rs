//! Re-export of the shared sysfs planner.
//!
//! The implementation lives in `uperf-config` (shared with `uperf-cli`). Re-export the
//! *planner* surface only: this module used to also re-export `SysfsWrite` from the crate
//! root, which is a different type from `crate::orchestrator::SysfsWrite` — two names, one
//! import away from a silent mix-up. Sinks that write use the orchestrator type.

pub use uperf_config::sysfs::{dispatch, plan_for_config, plan_scene, serialize_value};
pub use uperf_config::sysfs::{SysfsWrite, WriterKind};
