//! Small accessors that exist for a specific runtime decision, each with the
//! evidence for why they exist.

use uperf_config::Config;

/// `modules.log.level` — upstream has a `LogLevelSwitcher` class and the level
/// names `trace/debug/info/warn` as literals; all 63 shipped configs set
/// `"info"`. The build used to hardcode spdlog to debug.
#[test]
fn log_level_comes_from_modules() {
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {"log": {"level": "info"}},
        "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.log_level().as_deref(), Some("info"));

    // Absent module -> None, so the caller keeps its default rather than being
    // handed a made-up string.
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {},
        "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.log_level(), None);

    // Every level the binary knows about round-trips.
    for lv in ["trace", "debug", "info", "warn"] {
        let c = Config::from_value(serde_json::json!({
            "meta": {"name":"t","author":"t"},
            "modules": {"log": {"level": lv}},
            "initials": {}, "presets": {"balance": {"*": {}}}
        }))
        .unwrap();
        assert_eq!(c.log_level().as_deref(), Some(lv));
    }
}

/// `preset_names()` validates values that reference a preset from *outside* the
/// JSON: `cur_powermode.txt` and `perapp_powermode.txt`.
///
/// Order is alphabetical, not JSON order — `presets` is a `BTreeMap`. That is
/// harmless here (the name is only ever used for a membership test) but is worth
/// pinning so nobody builds order-dependent behaviour on it.
#[test]
fn preset_names_are_the_presets_keys_alphabetically() {
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {},
        "initials": {},
        "presets": {
            "balance": {"*": {}},
            "powersave": {"*": {}},
            "performance": {"*": {}},
            "fast": {"*": {}}
        }
    }))
    .unwrap();
    assert_eq!(
        c.preset_names(),
        vec!["balance", "fast", "performance", "powersave"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
}

/// `modules.input` thresholds, and the distribution across the shipped configs
/// that makes wiring them worth it.
#[test]
fn input_thresholds_are_read_and_the_shipped_values_differ_from_the_vendored_defaults() {
    // The values from a typical shipped config.
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {"input": {"enable": true, "swipeThd": 0.03, "gestureThdX": 0.03,
                              "gestureThdY": 0.03, "gestureDelayTime": 2.0,
                              "holdEnterTime": 1.0}},
        "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.input_thresholds(), Some((0.03f32, 0.03f32, 0.03f32)));
    assert_eq!(c.input_enabled(), Some(true));

    // sdm888.json (the alioch config) is the one that matches the vendored ctor.
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {"input": {"enable": true, "swipeThd": 0.01, "gestureThdX": 0.03,
                              "gestureThdY": 0.03, "gestureDelayTime": 2.0,
                              "holdEnterTime": 1.0}},
        "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.input_thresholds(), Some((0.01f32, 0.03f32, 0.03f32)));

    // Missing keys -> None rather than a silent 0.0, which would make every touch
    // a swipe.
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {"input": {"enable": true, "swipeThd": 0.03}},
        "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.input_thresholds(), None);

    // No input module at all.
    let c = Config::from_value(serde_json::json!({
        "meta": {"name":"t","author":"t"},
        "modules": {}, "initials": {}, "presets": {"balance": {"*": {}}}
    }))
    .unwrap();
    assert_eq!(c.input_thresholds(), None);
    assert_eq!(c.input_enabled(), None);
}

/// Over the real config tree: `swipeThd` is 0.03 everywhere except sdm888.json
/// (0.01, the value the vendored ctor hardcodes), while the other two thresholds
/// match the ctor everywhere. That is why the gap was invisible on alioth.
#[test]
fn the_shipped_swipe_threshold_distribution_is_what_justifies_the_wiring() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut files = 0usize;
    let Ok(entries) = std::fs::read_dir(root.join("config")) else {
        eprintln!("skipping: config/ not present");
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let Ok(c) = Config::from_value(v) else { continue };
        if let Some((swipe, gx, gy)) = c.input_thresholds() {
            files += 1;
            *counts.entry(format!("{swipe}")).or_default() += 1;
            assert_eq!((gx, gy), (0.03, 0.03), "{}", p.display());
        }
    }
    assert!(files >= 60, "expected ~63 configs, saw {files}");
    assert_eq!(
        counts.get("0.03").copied().unwrap_or(0),
        files - 1,
        "exactly one config should use the vendored default: {counts:?}"
    );
    assert_eq!(counts.get("0.01").copied().unwrap_or(0), 1, "{counts:?}");
}
