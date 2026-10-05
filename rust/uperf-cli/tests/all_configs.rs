//! AGENT.md §10.2 — load every *.json from docs/upstream-configs/ and assert the
//! parser accepts all of them AND produces a non-empty plan for at least one
//! mode/scene.

use std::path::PathBuf;

#[test]
fn all_v3_configs_parse_and_plan() {
    let cfg_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()           // rust/
        .parent().unwrap()           // uperf-rewrite/
        .join("docs/upstream-configs");
    let cfg_dir = match std::fs::canonicalize(&cfg_dir) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("skipping: docs/upstream-configs not vendored ({})", cfg_dir.display());
            return;
        }
    };

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&cfg_dir)
        .expect("read_dir failed")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    entries.sort();

    assert!(entries.len() >= 30, "expected ≥30 configs, found {}", entries.len());

    for entry in &entries {
        let bytes = std::fs::read(entry).unwrap();
        let cfg = uperf_cli::Config::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("parse failed for {}: {e}", entry.display()));
        assert!(!cfg.meta.name.is_empty(), "missing meta.name in {}", entry.display());
        assert!(!cfg.meta.author.is_empty(), "missing meta.author in {}", entry.display());
        let mode = cfg.presets.keys().next().expect("no presets").clone();
        let scenes = cfg.scenes_in_preset(&mode);
        assert!(!scenes.is_empty(), "no scenes in preset {} of {}", mode, entry.display());
        let _ = cfg.resolve(&mode, &scenes[0], "cpu.margin");
    }
}
