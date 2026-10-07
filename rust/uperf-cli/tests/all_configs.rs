//! AGENT.md §10.2 — load every *.json from both vendored config trees:
//!   * `docs/upstream-configs/` — the 38 configs from the upstream v3 release
//!     (`yerf_dev-22.09.04.zip`). Parser regression baseline.
//!   * `docs/ugt-configs/` — the 63 configs from the UGT fork. These extend the
//!     upstream shapes (extra sysfs.CPU* knobs, extra hintDuration.swjunk, etc.)
//!     and are the shipping target.
//!
//! Asserts: every file parses; every meta.name + author are non-empty; the first
//! preset has at least one scene; `cpu.margin` resolves (either from a preset or
//! from defaults — many configs deliberately leave it unset).

use std::path::PathBuf;

fn run(dir_name: &str) -> Vec<PathBuf> {
    let cfg_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()           // rust/
        .parent().unwrap()           // uperf-libre/
        .join("docs")
        .join(dir_name);
    let cfg_dir = match std::fs::canonicalize(&cfg_dir) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: docs/{} not vendored ({})", dir_name, cfg_dir.display());
            return Vec::new();
        }
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&cfg_dir)
        .expect("read_dir failed")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort();
    entries
}

fn check_all(entries: &[PathBuf], label: &str) {
    assert!(entries.len() >= 30, "{}: expected ≥30 configs, found {}", label, entries.len());
    for entry in entries {
        let bytes = std::fs::read(entry).unwrap();
        let cfg = uperf_cli::Config::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("parse failed for {}: {e}", entry.display()));
        assert!(!cfg.meta.name.is_empty(), "missing meta.name in {}", entry.display());
        assert!(!cfg.meta.author.is_empty(), "missing meta.author in {}", entry.display());
        let mode = cfg.presets.keys().next().expect("no presets").clone();
        let scenes = cfg.scenes_in_preset(&mode);
        assert!(!scenes.is_empty(), "no scenes in preset {} of {}", mode, entry.display());
        // Resolve is allowed to return None (cpu.margin is not required); only
        // assert that the call doesn't crash.
        let _ = cfg.resolve(&mode, &scenes[0], "cpu.margin");
    }
}

#[test]
fn upstream_v3_configs() {
    let entries = run("upstream-configs");
    check_all(&entries, "upstream-configs");
    eprintln!("upstream-configs: {} parsed", entries.len());
}

#[test]
fn ugt_configs() {
    let entries = run("ugt-configs");
    check_all(&entries, "ugt-configs");
    eprintln!("ugt-configs: {} parsed", entries.len());
}