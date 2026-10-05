//! `uperf-config` — the shared config layer (schema + cascade + sysfs planner).
//!
//! Used by `uperf-cli` (host parity tool) and `uperf-core` (the Android
//! staticlib). One copy, so the cascade semantics and the sysfs path templates
//! cannot drift between the tool that claims parity and the code that runs on
//! the device.

pub mod config;
pub mod cpu;
pub mod freq_target;
pub mod gov_build;
pub mod governor;
pub mod proc_stat;
pub mod sched;
pub mod switcher;
pub mod sysfs;

pub use config::{Config, CfgError, Meta, Modules, Preset, HINT_SCENES};
pub use cpu::PowerModel;
pub use freq_target::{freq_target_for_cluster, freq_writes, FreqTarget, RealFs};
pub use gov_build::{cpu_slices, freq_targets, freq_targets_with, governor_from_config, tunables_from};
pub use governor::{ClusterState, CpuJiffies, Governor, GovernorTunables};
pub use proc_stat::parse_stat;
pub use sched::{SchedConfig, SchedDecision, SchedError, SchedPlanner, SchedPolicy};
pub use switcher::{InodeMode, PerappAnomaly, PerappChoice, PerappRules, SwitcherConfig};
pub use sysfs::{dispatch, plan_for_config, plan_scene, serialize_value, SysfsWrite, WriterKind};
