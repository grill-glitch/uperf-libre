//! dfps-rs — dfps business logic, owned by dfps-rewrite.
//!
//! This is its **own crate** (`uperf-dfps`), mounted into uperf-libre as a subtree at
//! `rust/uperf-dfps`. The crate boundary is deliberate: it is the same boundary the
//! subtree uses, so "is this our code or theirs" is a fact of the build graph instead
//! of a convention. `uperf-core` reaches it through `pub use uperf_dfps as dfps_rs;`.
//!
//! **Do not edit in uperf-libre.** Make changes in grill-glitch/dfps-rewrite, then
//! `git subtree pull` here (or `git subtree push` from there to reverse-sync).
//!
//! Round-trip verified: a change committed in dfps-rewrite, then `git subtree split` +
//! push to `dfps-rs-split`, then
//! `git subtree pull --prefix=rust/uperf-dfps dfps-rs dfps-rs-split` in uperf-libre,
//! lands the change. The reorganisation that made this a crate of its own moved the
//! path from `rust/uperf-core/src/dfps_rs` to `rust/uperf-dfps`.
//!
//! Logging: the crate has no opinion about the sink — uperf installs one with
//! [`set_logger`] at startup (its `log_msg`), and without one the lines are dropped.
//!
//! Public surface (re-exported so uperf's `crate::DfpsTask` etc.
//! still resolves one level deep):
//!
//! * [`DfpsTask`] — the state machine + dedupe.
//! * [`notifier`] — Linux open() + write for `dfps_cur.txt`.
//! * [`FpsRule`], [`Tunables`], [`RuleTable`] from the `config` submodule.
//! * [`OFFSCREEN_PKG`], [`UNIVERSAL_PKG`] — special pkg name constants.

pub mod config;
pub mod logger;
pub mod notifier;
pub mod scheduler;
pub mod sys_settings;
pub mod task;

pub use config::{FpsRule, RuleTable, Tunables, OFFSCREEN_PKG, UNIVERSAL_PKG};
pub use logger::set_logger;
// Crate-internal alias for the four call sites that report to the host's log.
pub(crate) use logger::log;
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