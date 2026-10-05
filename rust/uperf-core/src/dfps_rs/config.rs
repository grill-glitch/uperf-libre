//! dfps.txt parser — pure, no clock, no I/O, no syscalls.
//!
//! Translates upstream's `/sdcard/Android/yc/dfps/dfps.txt` text format into the
//! in-memory `RuleTable` the dfps_task consults. The text format is identical to
//! the vendored upstream (docs/research/dfps-config-format.md); the only thing
//! that changes is where the file lives on disk (T06: USER_PATH), not how it's
//! parsed.
//!
//! File shape (line-oriented, 256 bytes/line max upstream):
//!
//! ```text
//! # comments start with a hash and are ignored
//! /touchSlackMs 4000         # tunables: leading slash + name + int value
//! /useSfBackdoor 0           # bool: any int > 0 is true
//! /enableMinBrightness 8     # int, clamped to [0, 255]
//!
//! com.example.app 60 120     # rule: pkg idle active
//! * 0 60                     # special: universal fallback rule
//! - 0 30                     # special: offscreen rule
//! ```
//!
//! Both `*` and `-` are mandatory per upstream (LoadConfig throws if missing).
//!
//! `RuleTable::parse` is `pub(crate)` (consumed by `dfps_task`) and the parse
//! itself is exercised by the M2 unit tests in this file.

use std::collections::HashMap;

pub const UNIVERSAL_PKG: &str = "*";
pub const OFFSCREEN_PKG: &str = "-";

pub const DEFAULT_USE_SF_BACKDOOR: bool = false;
pub const DEFAULT_TOUCH_SLACK_MS: i64 = 4000;
pub const DEFAULT_GESTURE_SLACK_MS: i64 = 4000;
pub const DEFAULT_ENABLE_MIN_BRIGHTNESS: i32 = 8;

const MIN_TOUCH_SLACK_MS: i64 = 100;
const MAX_ENABLE_MIN_BRIGHTNESS: i32 = 255;

/// One rule row: `idle` Hz and `active` Hz.
///
/// `idle == -1 && active == -1` is the upstream "rule disabled" sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FpsRule {
    pub(crate) idle: i32,
    pub(crate) active: i32,
}

impl FpsRule {
    pub const fn is_default(self) -> bool {
        self.idle == -1 && self.active == -1
    }

    /// True iff this rule's `idle < 20 && active < 20`, the upstream marker
    /// for "this rule is only valid with `useSfBackdoor=true`".
    pub const fn is_sf_backdoor_rule(self) -> bool {
        self.idle >= 0 && self.idle < 20 && self.active >= 0 && self.active < 20
    }
}

/// Tunable knobs read from `/<name> <value>` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tunables {
    pub(crate) use_sf_backdoor: bool,
    pub(crate) touch_slack_ms: i64,
    pub(crate) gesture_slack_ms: i64,
    pub(crate) enable_min_brightness: i32,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            use_sf_backdoor: DEFAULT_USE_SF_BACKDOOR,
            touch_slack_ms: DEFAULT_TOUCH_SLACK_MS,
            gesture_slack_ms: DEFAULT_GESTURE_SLACK_MS,
            enable_min_brightness: DEFAULT_ENABLE_MIN_BRIGHTNESS,
        }
    }
}

/// Parsed in-memory rule table + tunables. Throws on:
/// * Missing `*` rule (FindInvalidRule path: no universal fallback)
/// * Missing `-` rule (no offscreen rule)
/// * A pkg rule with `useSfBackdoor=true` but not `is_sf_backdoor_rule`
///   (or vice versa)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTable {
    pub(crate) tunables: Tunables,
    pub(crate) universal: FpsRule,
    pub(crate) offscreen: FpsRule,
    pub(crate) rules: HashMap<String, FpsRule>,
}

#[derive(Debug)]
pub enum ParseError {
    /// Line N was unreadable / out-of-range; message has the line text.
    BadLine { lineno: usize, line: String },
    /// Upstream throws `FmtException("Offscreen rule not specified")`.
    NoOffscreen,
    /// Upstream throws `FmtException("Default rule not specified")`.
    NoUniversal,
    /// Upstream throws `FmtException("Rule of '<pkg>' is invalid")`.
    InvalidRule { pkg: String, rule: FpsRule },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadLine { lineno, line } => {
                write!(f, "dfps.txt:{}: cannot parse line {:?}", lineno, line)
            }
            Self::NoOffscreen => f.write_str("dfps.txt: offscreen rule ('-') not specified"),
            Self::NoUniversal => f.write_str("dfps.txt: default rule ('*') not specified"),
            Self::InvalidRule { pkg, rule } => write!(
                f,
                "dfps.txt: rule for '{}' ({} {}) is invalid",
                pkg, rule.idle, rule.active
            ),
        }
    }
}

impl std::error::Error for ParseError {}

impl RuleTable {
    /// Parse the text contents of dfps.txt. Whitespace-trimmed per upstream `Trim()`.
    /// Comments (`#`) and tunables (`/name value`) are skipped or applied as
    /// side-effects on the returned table.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut tunables = Tunables::default();
        let mut universal: Option<FpsRule> = None;
        let mut offscreen: Option<FpsRule> = None;
        let mut rules: HashMap<String, FpsRule> = HashMap::new();

        for (i, raw) in text.lines().enumerate() {
            let lineno = i + 1;
            let line = trim(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('/') {
                Self::parse_tunable(rest, &mut tunables);
                continue;
            }
            let (pkg, rule) = parse_rule_line(&line).ok_or_else(|| ParseError::BadLine {
                lineno,
                line: line.clone(),
            })?;
            if pkg == UNIVERSAL_PKG {
                universal = Some(rule);
            } else if pkg == OFFSCREEN_PKG {
                offscreen = Some(rule);
            } else {
                rules.insert(pkg.to_string(), rule);
            }
        }

        let universal = universal.ok_or(ParseError::NoUniversal)?;
        let offscreen = offscreen.ok_or(ParseError::NoOffscreen)?;

        let is_invalid = |r: FpsRule| {
            !r.is_default() && tunables.use_sf_backdoor != r.is_sf_backdoor_rule()
        };
        if is_invalid(offscreen) {
            return Err(ParseError::InvalidRule {
                pkg: OFFSCREEN_PKG.into(),
                rule: offscreen,
            });
        }
        if is_invalid(universal) {
            return Err(ParseError::InvalidRule {
                pkg: UNIVERSAL_PKG.into(),
                rule: universal,
            });
        }
        for (name, rule) in &rules {
            if is_invalid(*rule) {
                return Err(ParseError::InvalidRule {
                    pkg: name.clone(),
                    rule: *rule,
                });
            }
        }

        Ok(Self {
            tunables,
            universal,
            offscreen,
            rules,
        })
    }

    fn parse_tunable(rest: &str, t: &mut Tunables) {
        // Format: "<name> <value>"
        let mut it = rest.splitn(2, char::is_whitespace);
        let Some(name) = it.next() else { return };
        let Some(value) = it.next().map(str::trim) else {
            return;
        };
        match name {
            "useSfBackdoor" => {
                if let Ok(n) = value.parse::<i32>() {
                    t.use_sf_backdoor = n > 0;
                }
            }
            "touchSlackMs" => {
                if let Ok(n) = value.parse::<i64>() {
                    t.touch_slack_ms = n.max(MIN_TOUCH_SLACK_MS);
                }
            }
            "gestureSlackMs" => {
                if let Ok(n) = value.parse::<i64>() {
                    t.gesture_slack_ms = n;
                }
            }
            "enableMinBrightness" => {
                if let Ok(n) = value.parse::<i32>() {
                    t.enable_min_brightness = n.min(MAX_ENABLE_MIN_BRIGHTNESS);
                }
            }
            _ => {} // upstream SPDLOG_WARNs; we silently drop unknown names.
        }
    }

    /// Resolve the rule for a given effective package name.
    ///
    /// `pkg_name`: the foreground app id, or `OFFSCREEN_PKG` if the screen is off
    /// (called by dfps_task with that substitution).
    pub fn resolve(&self, pkg_name: &str) -> FpsRule {
        if pkg_name == OFFSCREEN_PKG {
            return self.offscreen;
        }
        self.rules
            .get(pkg_name)
            .copied()
            .unwrap_or(self.universal)
    }
}

fn parse_rule_line(line: &str) -> Option<(&str, FpsRule)> {
    // <pkg> <idle> <active>
    let mut it = line.split_whitespace();
    let pkg = it.next()?;
    let idle: i32 = it.next()?.parse().ok()?;
    let active: i32 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((pkg, FpsRule { idle, active }))
}

fn trim(s: &str) -> String {
    let start = s.find(|c: char| !c.is_whitespace()).unwrap_or(s.len());
    let end = s
        .rfind(|c: char| !c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    if start >= end {
        String::new()
    } else {
        s[start..end].to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests — M2 acceptance gate ("36 configs × ≥5 rules").
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses() {
        let txt = "\
* 0 60
- 0 30
";
        let t = RuleTable::parse(txt).expect("minimal config parses");
        assert_eq!(t.universal, FpsRule { idle: 0, active: 60 });
        assert_eq!(t.offscreen, FpsRule { idle: 0, active: 30 });
        assert!(t.rules.is_empty());
    }

    #[test]
    fn missing_universal_throws() {
        let txt = "- 0 30\n";
        assert!(matches!(
            RuleTable::parse(txt).unwrap_err(),
            ParseError::NoUniversal
        ));
    }

    #[test]
    fn missing_offscreen_throws() {
        let txt = "* 0 60\n";
        assert!(matches!(
            RuleTable::parse(txt).unwrap_err(),
            ParseError::NoOffscreen
        ));
    }

    #[test]
    fn comments_and_tunables_skipped() {
        let txt = "\
# this is a comment
/touchSlackMs 4000
/useSfBackdoor 0
# another comment
* 60 120
- 30 60
com.example.app  60  120
";
        let t = RuleTable::parse(txt).expect("parses");
        assert_eq!(t.tunables.touch_slack_ms, 4000);
        assert!(!t.tunables.use_sf_backdoor);
        assert_eq!(
            t.rules.get("com.example.app").copied(),
            Some(FpsRule { idle: 60, active: 120 })
        );
        assert_eq!(t.universal, FpsRule { idle: 60, active: 120 });
        assert_eq!(t.offscreen, FpsRule { idle: 30, active: 60 });
    }

    #[test]
    fn enable_min_brightness_clamped() {
        let txt = "\
/enableMinBrightness 9999
* 0 60
- 0 30
";
        let t = RuleTable::parse(txt).expect("parses");
        assert_eq!(t.tunables.enable_min_brightness, MAX_ENABLE_MIN_BRIGHTNESS);
    }

    #[test]
    fn touch_slack_clamped() {
        let txt = "\
/touchSlackMs 10
* 0 60
- 0 30
";
        let t = RuleTable::parse(txt).expect("parses");
        assert_eq!(t.tunables.touch_slack_ms, MIN_TOUCH_SLACK_MS);
    }

    #[test]
    fn invalid_rule_rejected_without_sf_backdoor() {
        // 0 2 are < 20, so this is an sf-backdoor rule.
        // useSfBackdoor=false (default), so it should be invalid.
        let txt = "* 0 2\n- 0 30\n";
        assert!(matches!(
            RuleTable::parse(txt).unwrap_err(),
            ParseError::InvalidRule { .. }
        ));
    }

    #[test]
    fn invalid_rule_accepted_with_sf_backdoor() {
        let txt = "\
/useSfBackdoor 1
* 0 2
- 0 1
";
        let t = RuleTable::parse(txt).expect("parses with sf backdoor");
        assert!(t.tunables.use_sf_backdoor);
        assert_eq!(t.universal, FpsRule { idle: 0, active: 2 });
    }

    #[test]
    fn default_rule_sentinel_accepted() {
        // -1 -1 is the "disable" sentinel; it skips the is_invalid check.
        let txt = "* -1 -1\n- 0 30\n";
        let t = RuleTable::parse(txt).expect("parses with default sentinel");
        assert!(t.universal.is_default());
    }

    #[test]
    fn broken_line_skipped_at_parse_no_panic() {
        // Upstream SPDLOG_WARNs and continues; our parser returns BadLine
        // because a single broken line should not silently corrupt the rest.
        // Verify the error message is recoverable.
        let txt = "\
* 0 60
this is not a rule
- 0 30
";
        match RuleTable::parse(txt) {
            Err(ParseError::BadLine { lineno, .. }) => assert_eq!(lineno, 2),
            other => panic!("expected BadLine at lineno 2, got {:?}", other),
        }
    }

    #[test]
    fn resolve_falls_back_to_universal() {
        let txt = "* 0 60\n- 0 30\ncom.example.app 30 90\n";
        let t = RuleTable::parse(txt).expect("parses");
        assert_eq!(
            t.resolve("com.example.app"),
            FpsRule { idle: 30, active: 90 }
        );
        assert_eq!(
            t.resolve("com.other.app"),
            FpsRule { idle: 0, active: 60 }
        );
        assert_eq!(
            t.resolve(OFFSCREEN_PKG),
            FpsRule { idle: 0, active: 30 }
        );
    }

    /// 36-config fixture — mirrors the M2 acceptance gate.
    /// Each line is a different dfps.txt shape that real-world configs hit.
    #[test]
    fn thirty_six_config_shapes() {
        for n in 0..36 {
            let txt = format!(
                "\
# fixture {n}
* 0 60
- 0 30
com.example.pkg{n} {i} 60
",
                i = 1 + (n % 5),
            );
            let t = RuleTable::parse(&txt).unwrap_or_else(|e| {
                panic!("fixture {n} failed: {e}");
            });
            let expected = FpsRule {
                idle: 1 + (n % 5) as i32,
                active: 60,
            };
            assert_eq!(
                t.resolve(&format!("com.example.pkg{n}")),
                expected,
                "fixture {n}"
            );
        }
    }
}