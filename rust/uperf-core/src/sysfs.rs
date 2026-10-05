//! Re-export of the shared sysfs planner.
//!
//! The implementation lives in `uperf-config` (shared with `uperf-cli`) — this
//! module only keeps the `crate::sysfs::*` paths stable for existing callers.
//! A duplicate copy used to live here and drifted from the CLI's; do not
//! reintroduce one.

pub use uperf_config::sysfs::{dispatch, plan_for_config, plan_scene, serialize_value};
pub use uperf_config::{SysfsWrite, WriterKind};
