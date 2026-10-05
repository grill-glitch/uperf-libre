//! dfps-rs — dfps business logic, owned by dfps-rewrite, mounted into
//! uperf-rewrite as a subtree at this exact path.
//!
//! **Do not edit in uperf-rewrite.** Make changes here, then `git subtree
//! pull` from uperf-rewrite (or `git subtree push` from dfps-rewrite to
//! reverse-sync). See README in the repo root for the workflow.
//!
//! Public surface (re-exported so uperf's `crate::dfps_rs::DfpsTask` etc.
//! still resolves one level deep):
//!
//! * [`DfpsTask`] — the state machine + dedupe.
//! * [`notifier`] — Linux open() + write for `dfps_cur.txt`.
//! * [`FpsRule`], [`Tunables`], [`RuleTable`] from the `config` submodule.
//! * [`OFFSCREEN_PKG`], [`UNIVERSAL_PKG`] — special pkg name constants.

pub mod config;
pub mod notifier;
pub mod task;

pub use config::{FpsRule, RuleTable, Tunables, OFFSCREEN_PKG, UNIVERSAL_PKG};
pub use notifier::write_cur_hz;
pub use task::DfpsTask;

/// T06: reuse uperf-rs's USER_PATH so dfps state lives next to uperf state.
pub const DFPS_NOTIFY_PATH: &str = "/sdcard/Android/yc/uperf/dfps_cur.txt";