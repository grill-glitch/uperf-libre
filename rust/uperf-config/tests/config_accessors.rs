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
