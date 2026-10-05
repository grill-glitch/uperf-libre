//! Library re-export of the host-only modules so integration tests can drive them.
//!
//! The config layer + sysfs planner live in `uperf-config` (shared with the
//! on-device `uperf-core`). This crate only adds the CLI-facing `plan` emitter
//! and the `warn` reporter.

pub mod plan;
pub mod warn;

pub use uperf_config::{Config, CfgError, Meta, Modules, Preset};
