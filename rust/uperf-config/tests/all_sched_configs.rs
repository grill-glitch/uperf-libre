//! AGENT.md §10.2 — the sched acceptance gate over *every* vendored config.
//!
//! Walks `docs/upstream-configs/` (the 38 configs from the upstream v3 release)
//! and `docs/ugt-configs/` (the 63 UGT configs that are the shipping target) and
//! asserts, for each `modules.sched`:
//!
//!   * the module parses into `SchedConfig`;
//!   * every process `regex` and every thread `k` compiles with the `regex`
//!     crate after `/HOME_PACKAGE/` and `/MAIN_THREAD/` substitution — this is
//!     where the §3.2 "PCRE2 is required" assumption is settled, by data;
//!   * every `ac` exists in `affinity`, every `pc` exists in `prio`, every prio
//!     code decodes, and every cpumask name resolves — anything that does not is
//!     an `Anomaly` the planner tolerates, and the set of anomalies across the
//!     whole tree must equal EXACTLY the whitelist below. A new config with a new
//!     defect therefore fails here instead of silently doing nothing at runtime.

use std::path::PathBuf;

use uperf_config::{SchedConfig, SchedPlanner};

/// A stand-in launcher package / main-thread name. The point is that the
/// *substituted* text compiles; any plausible value exercises the same path the
/// device uses.
const SAMPLE_HOME: &str = "com.miui.home";

/// Configs that contain real defects upstream ships anyway, with their anomaly
/// count. Evidence: `sdm8g1+.json` is the reference config and defines
/// `affinity.fuck` + `prio.fuck`; `sdm8g2.json`/`sdm8g3.json` define
/// `prio.fuck` but forgot `affinity.fuck`; `sdm7g1.json` defines neither, yet
/// all three reference `ac: "fuck"` / `pc: "fuck"` in the same bloatware rule.
/// Tolerated as a no-op, exactly as upstream must tolerate them.
const KNOWN_ANOMALIES: [(&str, usize); 3] = [
    ("sdm7g1.json", 2),
    ("sdm8g2.json", 1),
    ("sdm8g3.json", 1),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn config_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["docs/upstream-configs", "docs/ugt-configs"] {
        let Ok(entries) = std::fs::read_dir(repo_root().join(dir)) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("json") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_shipped_config_compiles_and_only_the_known_defects_remain() {
    let files = config_files();
    assert!(
        files.len() >= 30,
        "expected the vendored config trees, found {} files",
        files.len()
    );

    let mut checked = 0usize;
    let mut patterns = 0usize;
    let mut rules_total = 0usize;
    let mut hard_failures: Vec<String> = Vec::new();
    let mut found_anomalies: Vec<(String, usize)> = Vec::new();

    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let Ok(bytes) = std::fs::read(f) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let Some(modules) = v.get("modules").and_then(|m| m.as_object()) else { continue };
        let Some(sched) = SchedConfig::from_modules(modules) else { continue };

        // Construction compiles every process regex and probes every thread
        // pattern. Only a broken pattern is fatal.
        match SchedPlanner::new(sched, SAMPLE_HOME) {
            Ok(planner) => {
                checked += 1;
                rules_total += planner.config().rules.len();
                for r in &planner.config().rules {
                    patterns += 1 + r.rules.len();
                }
                if !planner.anomalies().is_empty() {
                    found_anomalies.push((name.clone(), planner.anomalies().len()));
                }
            }
            Err(e) => hard_failures.push(format!("{name}: {e}")),
        }
    }

    assert!(
        hard_failures.is_empty(),
        "{} config(s) failed to compile:\n{}",
        hard_failures.len(),
        hard_failures.join("\n")
    );
    found_anomalies.sort();
    assert_eq!(
        found_anomalies,
        KNOWN_ANOMALIES
            .iter()
            .map(|(n, c)| (n.to_string(), *c))
            .collect::<Vec<_>>(),
        "the set of configs containing tolerated defects changed"
    );
    assert!(checked >= 30, "only {checked} sched modules were checked");
    println!(
        "checked {checked} sched modules, {rules_total} process rules, {patterns} patterns, \
         {} known-defect config(s)",
        found_anomalies.len()
    );
}

/// Independent of the planner: no shipped pattern needs a PCRE-only construct.
#[test]
fn no_shipped_pattern_needs_pcre2() {
    let pcre_only = [
        "(?=", "(?!", "(?<=", "(?<!", "(?>", "(?|", "(?(", "(?R", "\\K", "(?i)", "(?m)", "(?s)",
    ];
    let mut seen = std::collections::BTreeSet::new();
    for f in config_files() {
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let Some(sc) = v
            .get("modules")
            .and_then(|m| m.get("sched"))
            .and_then(|s| s.as_object())
        else {
            continue;
        };
        let mut pats: Vec<String> = Vec::new();
        for r in sc.get("rules").and_then(|r| r.as_array()).into_iter().flatten() {
            if let Some(p) = r.get("regex").and_then(|x| x.as_str()) {
                pats.push(p.to_string());
            }
            for it in r.get("rules").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(k) = it.get("k").and_then(|x| x.as_str()) {
                    pats.push(k.to_string());
                }
            }
        }
        for p in pats {
            seen.insert(p.clone());
            for needle in pcre_only {
                assert!(
                    !p.contains(needle),
                    "{}: {p:?} contains PCRE-only {needle:?}",
                    f.file_name().unwrap().to_string_lossy()
                );
            }
        }
    }
    assert!(!seen.is_empty(), "no patterns were collected");
    println!("unique shipped patterns: {}", seen.len());
}
