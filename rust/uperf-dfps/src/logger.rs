//! One logging hook, installed by the embedding binary.
//!
//! This code was written as a module inside `uperf-core` and called that crate's
//! `log_msg` directly. As a crate of its own it must not know the host's log sink, so
//! the host installs one function pointer at startup (`uperf_dfps::set_logger`).
//!
//! Dropping a line when nothing is installed is deliberate: the alternative is a
//! dependency on a global logger crate, and this code is also used by the host's tests,
//! where there is no sink at all.

use std::sync::OnceLock;

type Sink = fn(&str);

static SINK: OnceLock<Sink> = OnceLock::new();

/// Install the sink. The first call wins; later calls are ignored (the host has exactly
/// one log file).
pub fn set_logger(f: Sink) {
    let _ = SINK.set(f);
}

pub(crate) fn log(s: &str) {
    if let Some(f) = SINK.get() {
        f(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_without_a_sink_is_dropped_not_panicked() {
        // Cannot install twice in one process, so this only asserts the no-sink path is
        // safe — the process-wide install is exercised by the host's startup path.
        log("no sink installed");
    }
}
