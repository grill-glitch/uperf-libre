//! The frame-source ladder, and the one place a frame reading is published.
//!
//! Three legs can report a frame rate (AGENT.md §11 ④/⑤/⑪):
//!
//! 1. the M8-injected `sfanalysis.hint` file — a per-vsync event from inside
//!    SurfaceFlinger (not a task at all: the hint is read by the watcher),
//! 2. ⑪'s uprobe on the target app's own `libgui.so queueBuffer`, counted per thread
//!    (`sf_uprobe`),
//! 3. ⑤'s direct-binder `--latency` read (`sf_binder`), the degraded fallback.
//!
//! The ladder and the published reading used to live inside `sf_binder` — i.e. the
//! *fallback* owned the policy and reached into `sf_uprobe`'s module-level atomics to
//! ask whether the primary leg was healthy. That is backwards in the way that costs
//! later: a fourth leg would have meant editing the fallback's loop, and the shared
//! state was three globals with no owner. So:
//!
//!   * [`FrameSource`] and [`choose_source`] are the ladder, here;
//!   * [`publish_uprobe`] / [`publish_binder`] are how a leg reports, and [`active`] is
//!     how anything reads the winner (the recorder ⑧ and the state file both do);
//!   * [`write_state`] is the one writer of `<USER_PATH>/uperf_frames.state`.
//!
//! Legs still run their own threads with their own cadences; this module is only the
//! shared vocabulary and the shared last-value slot, which is why it holds no locks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Which frame source is primary.
///
/// The M8-injected hint wins whenever it is fresh — it is the real thing, a per-vsync
/// event from inside SurfaceFlinger. ⑪'s uprobe is next: it counts only the top app's
/// own frames. The direct-binder FPS leg is the **degraded fallback**; while either of
/// the other two is live it is not even polled, because each poll is a binder round trip
/// plus a pipe read — a cost decision, not a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSource {
    Hint,
    /// ⑪'s primary leg: a uprobe on the app's own `libgui.so` `queueBuffer`, counted per
    /// thread, so only that app's frames are counted.
    Uprobe,
    Fps,
}

impl FrameSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FrameSource::Hint => "hint",
            FrameSource::Uprobe => "uprobe",
            FrameSource::Fps => "fps",
        }
    }
}

/// What one leg last measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegReading {
    pub source: FrameSource,
    /// Frames per second, 0.0 when this leg has no reading.
    pub fps: f64,
    /// Frames counted in the last window.
    pub frames: u64,
    /// Whether this leg can be trusted right now (a working probe / a live connection).
    pub healthy: bool,
}

// ------------------------------------------------------------------ shared slots

mod slots {
    use super::*;

    /// ⑪'s last window, ×100 so it fits in an integer atomics slot.
    pub(super) static UPROBE_FPS_X100: AtomicU64 = AtomicU64::new(0);
    pub(super) static UPROBE_FRAMES: AtomicU64 = AtomicU64::new(0);
    /// Set while the uprobe leg has a healthy probe, so the binder leg stands down.
    pub(super) static UPROBE_HEALTHY: AtomicBool = AtomicBool::new(false);
    /// ⑤'s last measurement.
    pub(super) static BINDER_FPS_X100: AtomicU64 = AtomicU64::new(0);
    pub(super) static BINDER_CONNECTED: AtomicBool = AtomicBool::new(false);
    /// Set while the injected hint is the source in effect. It carries no fps of its own
    /// (one byte of hint), so while it is live the reading is "no fps", exactly as the
    /// recorder saw before the ladder moved here.
    pub(super) static HINT_LIVE: AtomicBool = AtomicBool::new(false);
}

/// ⑪ reports one window: `healthy` is whether its probe is installed and the count is
/// from this window (it is reset at the start of every window, so the value read at the
/// end *is* that window's count).
pub fn publish_uprobe(healthy: bool, fps: f64, frames: u64) {
    slots::UPROBE_HEALTHY.store(healthy, Ordering::Relaxed);
    slots::UPROBE_FPS_X100.store((fps.max(0.0) * 100.0) as u64, Ordering::Relaxed);
    slots::UPROBE_FRAMES.store(frames, Ordering::Relaxed);
}

pub fn uprobe() -> LegReading {
    LegReading {
        source: FrameSource::Uprobe,
        fps: slots::UPROBE_FPS_X100.load(Ordering::Relaxed) as f64 / 100.0,
        frames: slots::UPROBE_FRAMES.load(Ordering::Relaxed),
        healthy: slots::UPROBE_HEALTHY.load(Ordering::Relaxed),
    }
}

/// ⑤ reports its last window (and whether its binder link is up).
pub fn publish_binder(connected: bool, fps: f64) {
    slots::BINDER_CONNECTED.store(connected, Ordering::Relaxed);
    slots::BINDER_FPS_X100.store((fps.max(0.0) * 100.0) as u64, Ordering::Relaxed);
}

pub fn binder() -> LegReading {
    LegReading {
        source: FrameSource::Fps,
        fps: slots::BINDER_FPS_X100.load(Ordering::Relaxed) as f64 / 100.0,
        frames: 0,
        healthy: slots::BINDER_CONNECTED.load(Ordering::Relaxed),
    }
}

/// Whether the injected hint is the source in effect (it outranks both legs).
pub fn publish_hint(live: bool) {
    slots::HINT_LIVE.store(live, Ordering::Relaxed);
}

/// The reading that is actually in effect: the hint when it is live (no fps of its own),
/// else ⑪ when its probe is healthy, else ⑤.
///
/// The recorder (⑧) asks for this rather than for one named leg, so a session recorded
/// while the uprobe leg owns the number does not silently store the fallback's 0.0.
pub fn active() -> LegReading {
    if slots::HINT_LIVE.load(Ordering::Relaxed) {
        return LegReading {
            source: FrameSource::Hint,
            fps: 0.0,
            frames: 0,
            healthy: true,
        };
    }
    let u = uprobe();
    if u.healthy {
        u
    } else {
        binder()
    }
}

/// Last fps in effect, or 0.0 when no leg has a reading.
pub fn last_fps() -> f64 {
    active().fps
}

// ------------------------------------------------------------------------ ladder

/// Pick the primary source from the hint file's age. `None` (absent/unreadable) means
/// the hint leg is not there at all.
///
/// `Uprobe` is deliberately not returned here: whether it can serve is a property of the
/// probe (does the target app exist, did the symbol resolve), not of the hint's age. The
/// caller asks [`uprobe`] once the hint is not fresh — that is the ladder, and it is
/// exactly two steps so it stays readable.
pub fn choose_source(hint_age_ms: Option<u64>, stale_after_ms: u64) -> FrameSource {
    match hint_age_ms {
        Some(age) if age <= stale_after_ms => FrameSource::Hint,
        _ => FrameSource::Fps,
    }
}

// ---------------------------------------------------------------- published state

/// One line-set of `<USER_PATH>/uperf_frames.state`, as the WebUI and `webui.sh` read it.
#[derive(Debug, Clone, Copy)]
pub struct Published<'a> {
    pub source: FrameSource,
    /// Age of the injected hint, when that leg is the one in effect.
    pub hint_age_ms: Option<u64>,
    pub fps: Option<f64>,
    pub frames: usize,
    pub layer: Option<&'a str>,
    pub refresh_ns: Option<u64>,
}

impl Published<'_> {
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str(&format!("source={}\n", self.source.as_str()));
        out.push_str(&format!(
            "hint_age_ms={}\n",
            self.hint_age_ms
                .map(|a| a.to_string())
                .unwrap_or_else(|| "-".into())
        ));
        out.push_str(&format!(
            "fps={}\n",
            self.fps
                .map(|f| format!("{f:.1}"))
                .unwrap_or_else(|| "-".into())
        ));
        out.push_str(&format!("frames={}\n", self.frames));
        out.push_str(&format!(
            "refresh_ns={}\n",
            self.refresh_ns
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".into())
        ));
        out.push_str(&format!("layer={}\n", self.layer.unwrap_or("-")));
        out.push_str(&format!("ts_ms={}\n", now_ms()));
        out
    }
}

/// Write the published reading. Best effort and atomic-ish: reporting must never take a
/// frame leg down, so every failure here is silent.
pub fn write_state(path: Option<&Path>, p: &Published<'_>) {
    let Some(path) = path else { return };
    let lines = p.render();
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".new");
    let tmp = std::path::PathBuf::from(tmp);
    if std::fs::write(&tmp, &lines).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::write(path, &lines);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_picks_the_hint_while_it_is_fresh() {
        assert_eq!(choose_source(Some(0), 3000), FrameSource::Hint);
        assert_eq!(choose_source(Some(3000), 3000), FrameSource::Hint, "boundary is inclusive");
        assert_eq!(choose_source(Some(3001), 3000), FrameSource::Fps);
        assert_eq!(choose_source(None, 3000), FrameSource::Fps);
    }

    #[test]
    fn source_labels_are_the_ones_the_webui_shows() {
        assert_eq!(FrameSource::Hint.as_str(), "hint");
        assert_eq!(FrameSource::Uprobe.as_str(), "uprobe");
        assert_eq!(FrameSource::Fps.as_str(), "fps");
    }

    /// The recorder must record the leg that is actually in effect, not whichever one
    /// happens to be named.
    #[test]
    fn the_active_reading_prefers_a_healthy_uprobe() {
        publish_hint(false);
        publish_binder(true, 60.0);
        publish_uprobe(false, 0.0, 0);
        assert_eq!(active().source, FrameSource::Fps);
        assert_eq!(last_fps(), 60.0);

        publish_uprobe(true, 120.0, 120);
        assert_eq!(active().source, FrameSource::Uprobe);
        assert_eq!(last_fps(), 120.0);

        // A dead probe hands the number back to the binder leg.
        publish_uprobe(false, 0.0, 0);
        assert_eq!(active().source, FrameSource::Fps);
        assert_eq!(last_fps(), 60.0);

        // While the injected hint is live it outranks both legs, and it carries no fps —
        // the recorder stores 0.0 for that window (unchanged behaviour, now in one place).
        publish_hint(true);
        assert_eq!(active().source, FrameSource::Hint);
        assert_eq!(last_fps(), 0.0);
        publish_hint(false);
        assert_eq!(active().source, FrameSource::Fps);
    }

    #[test]
    fn published_state_has_every_field_the_webui_parses() {
        let p = Published {
            source: FrameSource::Uprobe,
            hint_age_ms: None,
            fps: Some(104.0),
            frames: 104,
            layer: None,
            refresh_ns: Some(8_333_333),
        };
        let text = p.render();
        for key in ["source=", "hint_age_ms=", "fps=", "frames=", "refresh_ns=", "layer=", "ts_ms="] {
            assert!(text.contains(key), "missing {key} in:\n{text}");
        }
        assert!(text.contains("source=uprobe"));
        assert!(text.contains("fps=104.0"));
        assert!(text.contains("hint_age_ms=-"), "an absent hint must render as '-', not 0");
        assert!(text.contains("layer=-"));
    }

    #[test]
    fn write_state_creates_and_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("uperf_frames_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("uperf_frames.state");
        write_state(Some(&path), &Published {
            source: FrameSource::Fps,
            hint_age_ms: Some(12),
            fps: Some(63.0),
            frames: 63,
            layer: Some("Layer#0"),
            refresh_ns: None,
        });
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("source=fps") && first.contains("layer=Layer#0"), "{first}");

        write_state(Some(&path), &Published {
            source: FrameSource::Hint,
            hint_age_ms: Some(3),
            fps: None,
            frames: 0,
            layer: None,
            refresh_ns: None,
        });
        let second = std::fs::read_to_string(&path).unwrap();
        assert!(second.contains("source=hint") && !second.contains("Layer#0"), "{second}");
        assert!(!path.with_extension("state.new").exists(), "temp file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
