//! Library re-export of the host-only modules so integration tests can drive them.
//!
//! The CLI binary itself is in `src/main.rs`.

pub mod config;
pub mod plan;
pub mod warn;

pub use config::{Config, Meta, Modules, Preset, CfgError};