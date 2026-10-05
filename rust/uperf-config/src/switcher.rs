//! Preset switching: `modules.switcher` + the two user-editable text files.
//!
//! Two files drive it, both plain text on `/sdcard`, both named by the config:
//!
//! * `switcher.switchInode` — `cur_powermode.txt`, written by the module's
//!   `powercfg_main.sh` as a single preset name:
//!   `echo "$1" > "$USER_PATH/cur_powermode.txt"` where `$1` is one of
//!   `powersave | balance | performance | fast | auto`. Upstream logs the value
//!   it reads as `Preset inode -> '<value>'`.
//! * `switcher.perapp` — `perapp_powermode.txt`, `<package> <preset>` per line,
//!   with `#` comments, `- <preset>` for the offscreen rule and `* <preset>` for
//!   the default rule. Verbatim from the module template:
//!
//!   ```text
//!   # 分应用性能模式配置
//!   # Per-app dynamic power mode rule
//!   # '-' means offscreen rule
//!   # '*' means default rule
//!
//!   com.tencent.tmgp.sgame performance
//!   - powersave
//!   * performance
//!   ```
//!
//! `auto` is a legal value of `cur_powermode.txt` but is **not** a preset name:
//! it is not among the config's `presets` keys (`balance/powersave/performance/
//! fast`). Upstream's strings `Internal perapp switcher {}` and
//! `Internal perapp switcher cannot be enabled` indicate it hands control to the
//! per-app rules. **[I]** — the exact semantics are inferred from those strings
//! plus the `powercfg_main.sh` value list, not observed end to end.

#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;

/// Preset names that `cur_powermode.txt` may hold per the module's
/// `powercfg_main.sh` case statement.
pub const INODE_MODES: [&str; 5] = ["powersave", "balance", "performance", "fast", "auto"];

/// What the inode file asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InodeMode {
    /// A concrete preset name from `cur_powermode.txt`.
    Preset(String),
    /// `auto` — defer to the per-app rules. **[I]** inferred, see module docs.
    Auto,
    /// A value that is neither a known preset nor `auto`; upstream logs
    /// `Failed to switch to undefined preset '<value>'`.
    Undefined(String),
}

impl InodeMode {
    /// Classify the trimmed contents of `cur_powermode.txt`.
    ///
    /// `known_presets` is the config's `presets` key set, so a preset the config
    /// does not define is reported rather than silently applied.
    pub fn classify(raw: &str, known_presets: &[String]) -> Option<Self> {
        let v = raw.trim();
        if v.is_empty() {
            return None;
        }
        if v == "auto" {
            return Some(Self::Auto);
        }
        if known_presets.iter().any(|p| p == v) {
            return Some(Self::Preset(v.to_string()));
        }
        Some(Self::Undefined(v.to_string()))
    }
}

/// `modules.switcher`'s two paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwitcherConfig {
    pub switch_inode: Option<String>,
    pub perapp: Option<String>,
}

impl SwitcherConfig {
    pub fn from_value(v: &Value) -> Self {
        let g = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            switch_inode: g("switchInode"),
            perapp: g("perapp"),
        }
    }

    pub fn from_modules(modules: &serde_json::Map<String, Value>) -> Option<Self> {
        Some(Self::from_value(modules.get("switcher")?))
    }
}

/// Why a preset was chosen — mirrors the shapes of upstream's log lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerappChoice {
    /// The screen is off and the `-` rule exists: `Perapp '-' -> '<preset>'`.
    Offscreen(String),
    /// An app rule matched: `Perapp '<package>' -> '<preset>'`.
    App { package: String, preset: String },
    /// The `*` rule: `Perapp '*' -> '<preset>'`.
    Default(String),
    /// Nothing applied; the caller keeps whatever mode is already in effect.
    None,
}

impl PerappChoice {
    pub fn preset(&self) -> Option<&str> {
        match self {
            Self::Offscreen(p) | Self::Default(p) => Some(p),
            Self::App { preset, .. } => Some(preset),
            Self::None => None,
        }
    }
}

/// A defect in `perapp_powermode.txt`, tolerated like the sched ones so a
/// user-edited file can never take the daemon down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerappAnomaly {
    /// `Perapp preset '<preset>' for app '<pkg>' not defined in config`
    UnknownPreset { package: String, preset: String },
    /// A non-comment line that is not `<key> <preset>`.
    MalformedLine { line: usize, text: String },
}

impl std::fmt::Display for PerappAnomaly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPreset { package, preset } => write!(
                f,
                "Perapp preset '{preset}' for app '{package}' not defined in config"
            ),
            Self::MalformedLine { line, text } => {
                write!(f, "perapp line {line} is not '<key> <preset>': {text:?}")
            }
        }
    }
}

/// The parsed `perapp_powermode.txt`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PerappRules {
    /// The `*` rule.
    pub default: Option<String>,
    /// The `-` rule, applied while the screen is off.
    pub offscreen: Option<String>,
    /// `<package> <preset>` rules, in file order.
    pub apps: BTreeMap<String, String>,
    /// Line order of `apps`, for deterministic logging.
    order: Vec<String>,
    pub anomalies: Vec<PerappAnomaly>,
}

impl PerappRules {
    /// Parse the file. `known_presets` are the config's `presets` keys; a rule
    /// naming anything else is recorded as an anomaly (upstream's
    /// `Perapp preset ... not defined in config`) and **dropped**, so an
    /// undefined preset can never be applied.
    pub fn parse(text: &str, known_presets: &[String]) -> Self {
        let mut me = Self::default();
        let known = |p: &str| known_presets.iter().any(|k| k == p);
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Split on the last whitespace run: packages never contain spaces,
            // and being lenient about extra columns keeps a hand-edited file
            // readable.
            let Some((key, preset)) = line.rsplit_once(char::is_whitespace) else {
                me.anomalies.push(PerappAnomaly::MalformedLine {
                    line: i + 1,
                    text: line.to_string(),
                });
                continue;
            };
            let key = key.trim();
            let preset = preset.trim();
            if key.is_empty() || preset.is_empty() {
                me.anomalies.push(PerappAnomaly::MalformedLine {
                    line: i + 1,
                    text: line.to_string(),
                });
                continue;
            }
            if !known(preset) {
                me.anomalies.push(PerappAnomaly::UnknownPreset {
                    package: key.to_string(),
                    preset: preset.to_string(),
                });
                continue;
            }
            match key {
                "*" => me.default = Some(preset.to_string()),
                "-" => me.offscreen = Some(preset.to_string()),
                pkg => {
                    if !me.apps.contains_key(pkg) {
                        me.order.push(pkg.to_string());
                    }
                    me.apps.insert(pkg.to_string(), preset.to_string());
                }
            }
        }
        me
    }

    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.offscreen.is_none() && self.apps.is_empty()
    }

    /// Which preset applies. Precedence — screen off wins, then an app rule,
    /// then the `*` rule. **[I]**: upstream's strings only prove that all three
    /// kinds exist and are looked up separately; the ordering is the natural
    /// reading ("'"-'" means offscreen rule").
    pub fn resolve(&self, top_app: Option<&str>, offscreen: bool) -> PerappChoice {
        if offscreen {
            if let Some(p) = &self.offscreen {
                return PerappChoice::Offscreen(p.clone());
            }
        }
        if let Some(app) = top_app {
            if let Some(p) = self.apps.get(app) {
                return PerappChoice::App {
                    package: app.to_string(),
                    preset: p.clone(),
                };
            }
        }
        match &self.default {
            Some(p) => PerappChoice::Default(p.clone()),
            None => PerappChoice::None,
        }
    }

    /// Packages in file order.
    pub fn packages(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn presets() -> Vec<String> {
        ["balance", "powersave", "performance", "fast"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// The exact module template, header comments included.
    const TEMPLATE: &str = "\
# 分应用性能模式配置
# Per-app dynamic power mode rule
# '-' means offscreen rule
# '*' means default rule

com.tencent.tmgp.sgame performance
com.miHoYo.Yuanshen performance
com.primatelabs.geekbench5 fast
- powersave
* performance
";

    #[test]
    fn parses_the_module_template() {
        let r = PerappRules::parse(TEMPLATE, &presets());
        assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
        assert_eq!(r.default.as_deref(), Some("performance"));
        assert_eq!(r.offscreen.as_deref(), Some("powersave"));
        assert_eq!(r.apps.get("com.miHoYo.Yuanshen").map(String::as_str), Some("performance"));
        assert_eq!(r.apps.get("com.primatelabs.geekbench5").map(String::as_str), Some("fast"));
        assert_eq!(r.apps.len(), 3, "the two comments and the blank line are not rules");
    }

    #[test]
    fn resolves_offscreen_first_then_app_then_default() {
        let r = PerappRules::parse(TEMPLATE, &presets());
        // Screen off wins even with a top app.
        assert_eq!(
            r.resolve(Some("com.miHoYo.Yuanshen"), true),
            PerappChoice::Offscreen("powersave".into())
        );
        // Screen on, app rule.
        assert_eq!(
            r.resolve(Some("com.miHoYo.Yuanshen"), false),
            PerappChoice::App {
                package: "com.miHoYo.Yuanshen".into(),
                preset: "performance".into()
            }
        );
        // Screen on, unknown app -> default rule.
        assert_eq!(
            r.resolve(Some("com.example.other"), false),
            PerappChoice::Default("performance".into())
        );
        // No top app at all -> default rule.
        assert_eq!(r.resolve(None, false), PerappChoice::Default("performance".into()));
    }

    #[test]
    fn offscreen_without_a_rule_falls_through_to_the_app_rule() {
        let r = PerappRules::parse("com.a fast\n* balance\n", &presets());
        assert_eq!(r.offscreen, None);
        assert_eq!(
            r.resolve(Some("com.a"), true),
            PerappChoice::App { package: "com.a".into(), preset: "fast".into() },
            "a missing '-' rule must not blank the mode"
        );
    }

    #[test]
    fn missing_default_is_reported_as_no_choice() {
        let r = PerappRules::parse("com.a fast\n- powersave\n", &presets());
        assert_eq!(r.default, None);
        assert_eq!(r.resolve(Some("com.unknown"), false), PerappChoice::None);
    }

    #[test]
    fn undefined_preset_is_recorded_and_dropped() {
        let r = PerappRules::parse("com.a turbo\n* balance\n", &presets());
        assert_eq!(r.apps.get("com.a"), None, "an undefined preset must not be applied");
        assert_eq!(
            r.anomalies,
            vec![PerappAnomaly::UnknownPreset {
                package: "com.a".into(),
                preset: "turbo".into()
            }]
        );
        // The message matches upstream's format string verbatim.
        assert_eq!(
            r.anomalies[0].to_string(),
            "Perapp preset 'turbo' for app 'com.a' not defined in config"
        );
    }

    #[test]
    fn malformed_lines_are_recorded_not_fatal() {
        let r = PerappRules::parse("justoneword\n\ncom.a  fast\n", &presets());
        assert_eq!(r.apps.get("com.a").map(String::as_str), Some("fast"));
        assert_eq!(r.anomalies.len(), 1);
        assert!(matches!(r.anomalies[0], PerappAnomaly::MalformedLine { line: 1, .. }));
    }

    #[test]
    fn windows_line_endings_and_extra_spaces_are_tolerated() {
        // A hand-edited /sdcard file may well have CRLF.
        let r = PerappRules::parse("com.a   fast\r\n*\tbalance\r\n", &presets());
        assert_eq!(r.apps.get("com.a").map(String::as_str), Some("fast"), "{:?}", r.anomalies);
        assert_eq!(r.default.as_deref(), Some("balance"));
        assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    }

    #[test]
    fn last_duplicate_rule_wins_and_order_is_kept() {
        let r = PerappRules::parse("com.a fast\ncom.a balance\ncom.b fast\n", &presets());
        assert_eq!(r.apps.get("com.a").map(String::as_str), Some("balance"));
        assert_eq!(r.packages().cloned().collect::<Vec<_>>(), vec!["com.a", "com.b"]);
    }

    #[test]
    fn parses_the_real_module_templates() {
        // Both the upstream release and the UGT fork ship a template; parse both
        // and check every referenced preset exists.
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let mut seen = 0;
        for p in [
            root.join("magisk/config/perapp_powermode.txt"),
            // the extracted upstream release, if present
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".hermes/cache/scratch/uperf_re/uperf/config/perapp_powermode.txt"),
        ] {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            seen += 1;
            let r = PerappRules::parse(&text, &presets());
            assert!(
                r.anomalies.is_empty(),
                "{}: {:?}",
                p.display(),
                r.anomalies
            );
            assert!(r.default.is_some(), "{}: template must carry a '*' rule", p.display());
            assert!(r.offscreen.is_some(), "{}: template must carry a '-' rule", p.display());
            assert!(!r.apps.is_empty(), "{}: template must carry app rules", p.display());
        }
        assert!(seen >= 1, "expected at least the vendored template");
    }

    #[test]
    fn inode_modes_classify_the_powercfg_values() {
        let p = presets();
        assert_eq!(
            InodeMode::classify("balance\n", &p),
            Some(InodeMode::Preset("balance".into()))
        );
        assert_eq!(InodeMode::classify("  fast  ", &p), Some(InodeMode::Preset("fast".into())));
        assert_eq!(InodeMode::classify("auto", &p), Some(InodeMode::Auto));
        assert_eq!(
            InodeMode::classify("turbo", &p),
            Some(InodeMode::Undefined("turbo".into()))
        );
        assert_eq!(InodeMode::classify("", &p), None);
        assert_eq!(InodeMode::classify("\n", &p), None);
        // The exact value list from powercfg_main.sh.
        for v in INODE_MODES {
            assert!(InodeMode::classify(v, &p).is_some(), "{v}");
        }
    }

    #[test]
    fn switcher_paths_come_from_the_config() {
        let c = SwitcherConfig::from_value(&json!({
            "switchInode": "/sdcard/Android/yc/uperf/cur_powermode.txt",
            "perapp": "/sdcard/Android/yc/uperf/perapp_powermode.txt",
            "hintDuration": {"idle": 0.0}
        }));
        assert_eq!(
            c.switch_inode.as_deref(),
            Some("/sdcard/Android/yc/uperf/cur_powermode.txt")
        );
        assert_eq!(c.perapp.as_deref(), Some("/sdcard/Android/yc/uperf/perapp_powermode.txt"));
        // Absent / blank values are None, not "".
        let c = SwitcherConfig::from_value(&json!({"switchInode": "  "}));
        assert_eq!(c.switch_inode, None);
        assert_eq!(c.perapp, None);
    }
}
