//! dfps-rs — dfps business logic, owned by dfps-rewrite, mounted into
//! uperf-rewrite as a subtree at this exact path.
//!
//! **Do not edit in uperf-rewrite.** Make changes here, then
//! `git subtree pull` from uperf-rewrite (or `git subtree push` from dfps-rewrite to
//! reverse-sync). See `docs/subtree-workflow.md` in this repo for the full recipe.
//!
//! Round-trip verified: a change committed here, then `git subtree split` +
//! push to `dfps-rs-split`, then `git subtree pull --prefix=rust/uperf-core/src/dfps_rs
//! dfps-rs dfps-rs-split` in uperf-rewrite, lands the change. See
//! `docs/subtree-workflow.md` §Verification.
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
pub mod scheduler;
pub mod sys_settings;
pub mod task;

pub use config::{FpsRule, RuleTable, Tunables, OFFSCREEN_PKG, UNIVERSAL_PKG};
pub use notifier::write_cur_hz;
pub use scheduler::{DfpsScheduler, RealSink, RecordingSink, RefreshSink};
pub use task::DfpsTask;

/// T06: reuse uperf-rs's USER_PATH so dfps state lives next to uperf state.
pub const DFPS_NOTIFY_PATH: &str = "/sdcard/Android/yc/uperf/dfps_cur.txt";

/// Basename of the dfps rule table. Lives in the same directory as
/// `uperf.json` (AGENT.md §7.1); uperf-rs resolves the directory from its own
/// config path, so only the name lives here.
pub const DFPS_CONFIG_FILE: &str = "dfps.txt";

/// Parse `dfps.txt` text into a table. The one entry point uperf-rs uses when
/// the inotify watcher reports a write, so the boot and reload paths cannot
/// drift.
pub fn parse_config(text: &str) -> Result<config::RuleTable, config::ParseError> {
    config::RuleTable::parse(text)
}