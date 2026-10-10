//! Recording channel (AGENT.md §11 queue item ⑧) — the rules, and the store.
//!
//! The rules come from AppOpt's history store, and they are the whole point:
//!
//! * **opt-in** — nothing is recorded unless it is switched on;
//! * **session ≥ 3 min** — shorter foreground runs are noise, so they are dropped rather
//!   than averaged in;
//! * **Deflate** — the store is raw-DEFLATE compressed;
//! * **dedupe by `pkg` + `epoch`** — the same package's same session is never written
//!   twice (a restart mid-session must not double-count).
//!
//! A session is one foreground stretch of one package: `{pkg, epoch_ms, duration_ms,
//! samples[]}` where a sample is `{t_ms, scene, fps}`. The daemon is the only reader
//! (Deflate is not something the shell surface can inflate), which is why the store is
//! private to it; exposing it is a later, deliberate step.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Shorter sessions are dropped: the rule from the queue, not a tunable.
pub const MIN_SESSION_MS: u64 = 180_000;

/// One observation inside an open session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// milliseconds since the session opened
    pub t_ms: u64,
    pub scene: String,
    /// last measured fps; 0.0 when the direct-binder frame leg is not running
    pub fps: f64,
}

/// One foreground stretch of one package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub pkg: String,
    /// session start, ms since boot — the `epoch` of the `pkg`+`epoch` key
    pub epoch_ms: u64,
    pub duration_ms: u64,
    pub samples: Vec<Sample>,
}

impl Session {
    /// The dedupe key (also the file name stem).
    pub fn key(&self) -> String {
        format!("{}-{}", sanitize(&self.pkg), self.epoch_ms)
    }
}

/// A package name is not a safe path component as-is; keep the characters that are, and
/// replace the rest. Deterministic, so the same pkg always maps to the same file.
pub fn sanitize(pkg: &str) -> String {
    let mut out = String::with_capacity(pkg.len());
    for c in pkg.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Raw DEFLATE (no zlib or gzip wrapper) of the JSON form.
pub fn encode(sessions: &[Session]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(sessions).map_err(|e| format!("serialize: {e}"))?;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&json).map_err(|e| format!("deflate: {e}"))?;
    enc.finish().map_err(|e| format!("deflate finish: {e}"))
}

/// Inverse of [`encode`]. A truncated or non-DEFLATE blob is an error, never a partial
/// guess.
pub fn decode(bytes: &[u8]) -> Result<Vec<Session>, String> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(bytes)
        .read_to_end(&mut out)
        .map_err(|e| format!("inflate: {e}"))?;
    serde_json::from_slice(&out).map_err(|e| format!("parse: {e}"))
}

/// The live recorder: open session, rules, commit.
pub struct Recorder {
    enabled: bool,
    min_session_ms: u64,
    sample_every_ms: u64,
    dir: Option<PathBuf>,
    open: Option<Open>,
    seen: HashSet<String>,
}

struct Open {
    pkg: String,
    started_ms: u64,
    last_sample_ms: u64,
    samples: Vec<Sample>,
}

impl Recorder {
    /// `dir` is where `history/` is created; `None` records in memory only (used by the
    /// tests, and by a run where no store location is known).
    pub fn new(enabled: bool, min_session_ms: u64, sample_every_ms: u64, dir: Option<PathBuf>) -> Self {
        let mut seen = HashSet::new();
        if let Some(d) = dir.as_deref() {
            if let Ok(rd) = std::fs::read_dir(history_dir(d)) {
                for e in rd.flatten() {
                    if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                        seen.insert(stem.to_string());
                    }
                }
            }
        }
        Self {
            enabled,
            min_session_ms,
            sample_every_ms: sample_every_ms.max(1),
            dir,
            open: None,
            seen,
        }
    }

    /// From the environment: `UPERF_RECORD=1`, `UPERF_RECORD_MIN_MS`,
    /// `UPERF_RECORD_SAMPLE_MS`, store under the config dir.
    pub fn from_env(cfg_dir: Option<&Path>) -> Recorder {
        let enabled = matches!(
            std::env::var("UPERF_RECORD").ok().as_deref(),
            Some("1") | Some("true")
        );
        let min = std::env::var("UPERF_RECORD_MIN_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(MIN_SESSION_MS);
        let every = std::env::var("UPERF_RECORD_SAMPLE_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1000);
        Recorder::new(enabled, min, every, cfg_dir.map(|d| d.to_path_buf()))
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Feed one tick. Returns a session that became complete *and* is worth keeping
    /// (the caller commits it). The package leaving the foreground, or becoming unknown,
    /// both close the open session.
    pub fn observe(
        &mut self,
        now_ms: u64,
        top_app: Option<&str>,
        scene: &str,
        fps: f64,
    ) -> Option<Session> {
        if !self.enabled {
            return None;
        }
        match top_app {
            None => self.close(now_ms),
            Some(pkg) => {
                let switched = self.open.as_ref().map(|o| o.pkg != pkg).unwrap_or(false);
                if switched {
                    let done = self.close(now_ms);
                    self.start(now_ms, pkg);
                    return done;
                }
                if self.open.is_none() {
                    self.start(now_ms, pkg);
                }
                if let Some(o) = self.open.as_mut() {
                    if now_ms.saturating_sub(o.last_sample_ms) >= self.sample_every_ms {
                        o.last_sample_ms = now_ms;
                        o.samples.push(Sample {
                            t_ms: now_ms.saturating_sub(o.started_ms),
                            scene: scene.to_string(),
                            fps,
                        });
                    }
                }
                None
            }
        }
    }

    fn start(&mut self, now_ms: u64, pkg: &str) {
        self.open = Some(Open {
            pkg: pkg.to_string(),
            started_ms: now_ms,
            // force a sample on the first tick after the switch
            last_sample_ms: now_ms.saturating_sub(self.sample_every_ms),
            samples: Vec::new(),
        });
    }

    fn close(&mut self, now_ms: u64) -> Option<Session> {
        let o = self.open.take()?;
        let duration_ms = now_ms.saturating_sub(o.started_ms);
        if duration_ms < self.min_session_ms {
            return None; // the "session ≥ 3 min" rule: too short is noise, not data
        }
        Some(Session {
            pkg: o.pkg,
            epoch_ms: o.started_ms,
            duration_ms,
            samples: o.samples,
        })
    }

    /// Write a kept session, unless its `pkg`+`epoch` is already known. Returns the path
    /// written, or `Ok(None)` when the dedupe rule skipped it.
    pub fn commit(&mut self, s: &Session) -> Result<Option<PathBuf>, String> {
        let key = s.key();
        if self.seen.contains(&key) {
            return Ok(None);
        }
        let Some(dir) = self.dir.as_ref() else {
            self.seen.insert(key);
            return Ok(None);
        };
        let hd = history_dir(dir);
        std::fs::create_dir_all(&hd).map_err(|e| format!("mkdir {}: {e}", hd.display()))?;
        let path = hd.join(format!("{key}.dfl"));
        let bytes = encode(std::slice::from_ref(s))?;
        // write through a temp file so a reader never sees half a record
        let tmp = hd.join(format!("{key}.dfl.tmp"));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(&path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        }
        self.seen.insert(key);
        Ok(Some(path))
    }

    /// The count of keys this recorder knows about (on disk at startup, plus committed).
    pub fn known_sessions(&self) -> usize {
        self.seen.len()
    }
}

fn history_dir(dir: &Path) -> PathBuf {
    dir.join("history")
}

// ---------------------------------------------------------------- daemon task

fn mono_ms() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000
}

/// The recording task. Opt-in: `UPERF_RECORD=1`. Ticks at 1 Hz (the sample cadence),
/// reads `(top_app, scene)` from the orchestrator and the fps the frame leg last
/// measured, and commits a session when one closes and passes the ≥3 min rule.
pub struct RecorderTask {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RecorderTask {
    pub fn enabled() -> bool {
        matches!(std::env::var("UPERF_RECORD").ok().as_deref(), Some("1") | Some("true"))
    }

    pub fn spawn<F, L>(cfg_dir: Option<PathBuf>, state: F, log: L) -> Option<RecorderTask>
    where
        F: Fn() -> (Option<String>, String) + Send + 'static,
        L: Fn(&str) + Send + 'static,
    {
        if !Self::enabled() {
            return None;
        }
        let mut rec = Recorder::from_env(cfg_dir.as_deref());
        let tick_ms = std::env::var("UPERF_RECORD_TICK_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1000);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("uperf-record".into())
            .spawn(move || {
                log(&format!(
                    "Rust: recorder enabled (min session {} ms, known {})",
                    rec.min_session_ms,
                    rec.known_sessions()
                ));
                loop {
                    let mut slept = 0u64;
                    while slept < tick_ms && !stop_thread.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        slept += 50;
                    }
                    let now = mono_ms();
                    if stop_thread.load(Ordering::SeqCst) {
                        // close whatever is open before leaving
                        if let Some(s) = rec.observe(now, None, "idle", 0.0) {
                            commit_and_log(&mut rec, &s, &log);
                        }
                        break;
                    }
                    let (top, scene) = state();
                    let fps = crate::sf_binder::last_fps();
                    if let Some(s) = rec.observe(now, top.as_deref(), &scene, fps) {
                        commit_and_log(&mut rec, &s, &log);
                    }
                }
                log("Rust: recorder stopped");
            })
            .ok()?;
        Some(RecorderTask { stop, handle: Some(handle) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn commit_and_log(rec: &mut Recorder, s: &Session, log: &impl Fn(&str)) {
    match rec.commit(s) {
        Ok(Some(p)) => log(&format!(
            "Rust: recorder session {} {} ms ({} samples) -> {}",
            s.pkg,
            s.duration_ms,
            s.samples.len(),
            p.display()
        )),
        Ok(None) => log(&format!("Rust: recorder session {}/{} already recorded", s.pkg, s.epoch_ms)),
        Err(e) => log(&format!("Rust: recorder write failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("uperf_rec_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rec(enabled: bool, min_ms: u64, dir: Option<PathBuf>) -> Recorder {
        Recorder::new(enabled, min_ms, 1000, dir)
    }

    #[test]
    fn disabled_records_nothing() {
        let mut r = rec(false, 1000, None);
        assert!(r.observe(0, Some("com.a"), "touch", 60.0).is_none());
        assert!(r.observe(10_000, Some("com.a"), "touch", 60.0).is_none());
        assert!(r.observe(20_000, None, "idle", 0.0).is_none());
        assert_eq!(r.known_sessions(), 0);
    }

    #[test]
    fn a_short_session_is_dropped_and_a_long_one_kept() {
        // 3 minutes is the rule
        let mut r = rec(true, 180_000, None);
        r.observe(0, Some("com.a"), "touch", 55.0);
        // 2:59 then a switch -> too short, dropped
        assert!(r.observe(179_000, Some("com.b"), "idle", 0.0).is_none());
        // one tick inside com.b (the switch tick itself only opens the session)
        r.observe(180_000, Some("com.b"), "idle", 0.0);
        // com.b runs a full 3:00 -> kept
        let s = r.observe(359_000, Some("com.c"), "idle", 0.0).expect("com.b is long enough");
        assert_eq!(s.pkg, "com.b");
        assert_eq!(s.duration_ms, 180_000);
        assert_eq!(s.epoch_ms, 179_000);
        // samples come from ticks, one per cadence
        assert_eq!(s.samples.len(), 1, "{:?}", s.samples);
        assert_eq!(s.samples[0].t_ms, 1000);
    }

    #[test]
    fn the_app_leaving_the_foreground_closes_the_session() {
        let mut r = rec(true, 1000, None);
        r.observe(0, Some("com.a"), "touch", 60.0);
        let s = r.observe(5000, None, "idle", 0.0).expect("closed on unknown top app");
        assert_eq!(s.pkg, "com.a");
        assert_eq!(s.duration_ms, 5000);
        // and nothing is open now
        assert!(r.observe(6000, None, "idle", 0.0).is_none());
    }

    #[test]
    fn samples_are_limited_by_the_cadence_and_timed_from_the_epoch() {
        let mut r = rec(true, 0, None);
        r.observe(1000, Some("com.a"), "idle", 0.0);
        r.observe(1200, Some("com.a"), "touch", 60.0); // too soon
        r.observe(2300, Some("com.a"), "touch", 60.0); // 1.3 s later
        let s = r.observe(4000, None, "idle", 0.0).unwrap();
        assert_eq!(s.epoch_ms, 1000);
        assert_eq!(s.samples.len(), 2, "{:?}", s.samples);
        assert_eq!(s.samples[0].t_ms, 0);
        assert_eq!(s.samples[1].t_ms, 1300);
        assert_eq!(s.samples[1].scene, "touch");
        assert_eq!(s.samples[1].fps, 60.0);
    }

    #[test]
    fn deflate_round_trips_and_is_actually_compressed() {
        let s = Session {
            pkg: "com.example.app".into(),
            epoch_ms: 12345,
            duration_ms: 200_000,
            samples: (0..200)
                .map(|i| Sample { t_ms: i * 1000, scene: "touch".into(), fps: 59.0 })
                .collect(),
        };
        let bytes = encode(std::slice::from_ref(&s)).unwrap();
        let json = serde_json::to_vec(std::slice::from_ref(&s)).unwrap();
        assert!(bytes.len() < json.len(), "deflate must actually compress");
        // raw DEFLATE, not zlib: the zlib header would be 0x78
        assert_ne!(bytes[0], 0x78, "expected raw deflate, got a zlib wrapper");
        assert_eq!(decode(&bytes).unwrap(), vec![s]);
        // and garbage is an error, not a partial guess
        assert!(decode(b"not deflate at all").is_err());
    }

    #[test]
    fn commit_dedupes_by_pkg_and_epoch() {
        let dir = tmp_dir("dedupe");
        let mut r = rec(true, 0, Some(dir.clone()));
        let s = Session { pkg: "com.a".into(), epoch_ms: 999, duration_ms: 200_000, samples: vec![] };
        let p1 = r.commit(&s).unwrap();
        assert!(p1.is_some(), "the first write lands");
        let path = p1.unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with("com.a-999.dfl"), "{path:?}");
        // the same pkg+epoch again is skipped
        assert_eq!(r.commit(&s).unwrap(), None);
        // a different epoch of the same package is a different session
        let s2 = Session { epoch_ms: 1000, ..s.clone() };
        assert!(r.commit(&s2).unwrap().is_some());
        // and a fresh recorder sees the existing keys and still dedupes
        let mut r2 = rec(true, 0, Some(dir.clone()));
        assert_eq!(r2.known_sessions(), 2);
        assert_eq!(r2.commit(&s).unwrap(), None);
        // the stored file really decodes back
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(decode(&bytes).unwrap()[0].pkg, "com.a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_path_safe() {
        assert_eq!(sanitize("com.example/app:1"), "com.example_app_1");
        assert_eq!(sanitize(""), "_");
        let s = Session { pkg: "a/b".into(), epoch_ms: 7, duration_ms: 0, samples: vec![] };
        assert_eq!(s.key(), "a_b-7");
    }

    #[test]
    fn a_recorder_with_no_directory_keeps_the_dedupe_in_memory() {
        let mut r = rec(true, 0, None);
        let s = Session { pkg: "com.a".into(), epoch_ms: 1, duration_ms: 0, samples: vec![] };
        assert_eq!(r.commit(&s).unwrap(), None, "nowhere to write");
        assert_eq!(r.known_sessions(), 1);
        assert_eq!(r.commit(&s).unwrap(), None, "still deduped");
    }
}
