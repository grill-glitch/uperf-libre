//! `uperf-config` — the shared config layer (schema + cascade + sysfs planner).
//!
//! Used by `uperf-cli` (host parity tool) and `uperf-core` (the Android
//! staticlib). One copy, so the cascade semantics and the sysfs path templates
//! cannot drift between the tool that claims parity and the code that runs on
//! the device.

pub mod config;
pub mod sysfs;

pub use config::{Config, CfgError, Meta, Modules, Preset, HINT_SCENES};
pub use sysfs::{dispatch, plan_for_config, plan_scene, serialize_value, SysfsWrite, WriterKind};
