//! CPU energy model + userspace governor math.
//!
//! Reverse-engineered from the upstream binary's startup log, which prints the
//! model's own numbers for every OPP:
//!
//! ```text
//! CpuGovernor cluster0:
//! opp  614400 pwr 0.053 cost 0.075
//! ...
//! opp 1804800 pwr 0.302 cost 0.146
//! ```
//!
//! Fit (25/25 OPPs across 3 clusters match to <0.0015; see the golden tests):
//!
//! ```text
//! x  = freq_GHz / typicalFreq
//! Rp = plainFreq / typicalFreq
//! Rs = sweetFreq / typicalFreq
//!
//!            ⎧ Rs·Rp·x     x < Rp      (linear through the origin)
//! ratio(x) = ⎨ Rs·x²       x < Rs      (quadratic between plainFreq and sweetFreq)
//!            ⎩ x³          x ≥ Rs      (cubic; extrapolates past typicalFreq)
//!
//! power = typicalPower · ratio(x)                       [W, per core]
//! cost  = power / ((efficiency/100) · freq_GHz)         [W / (relative GHz)]
//! ```
//!
//! `efficiency` is Cortex-A53@1.0 GHz = 100 (per `config/README.md`), so
//! `efficiency/100 · freq` is the core's relative performance.
//!
//! `freeFreq` does NOT appear in the fit — it is documented in `config/README.md`
//! as "单核最低功耗频点" (the lowest-power OPP), i.e. a floor for capacity/guideCap
//! reasoning rather than a term in the power curve. Kept in the struct and used
//! by the governor's capacity step; flagged here so the next reader doesn't
//! assume it was dropped by accident.

#![allow(dead_code)]

use serde_json::Value;

/// One cluster's energy model, as shipped in `modules.cpu.powerModel[]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerModel {
    /// Single-core relative performance (Cortex-A53@1.0GHz = 100).
    pub efficiency: f64,
    /// Cores in this cluster.
    pub nr: u32,
    /// Single-core typical power (W).
    pub typical_power: f64,
    /// Single-core typical frequency (GHz) — the model's calibration point.
    pub typical_freq: f64,
    /// Single-core sweet-spot junction frequency (GHz).
    pub sweet_freq: f64,
    /// Single-core linear junction frequency (GHz).
    pub plain_freq: f64,
    /// Single-core lowest-power frequency (GHz).
    pub free_freq: f64,
}

impl PowerModel {
    pub fn from_value(v: &Value) -> Option<Self> {
        let g = |k: &str| v.get(k).and_then(|x| x.as_f64());
        let eff = g("efficiency")?;
        if eff <= 0.0 {
            return None;
        }
        let m = Self {
            efficiency: eff,
            nr: v.get("nr").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
            typical_power: g("typicalPower")?,
            typical_freq: g("typicalFreq")?,
            sweet_freq: g("sweetFreq").unwrap_or(0.0),
            plain_freq: g("plainFreq").unwrap_or(0.0),
            free_freq: g("freeFreq").unwrap_or(0.0),
        };
        if m.typical_freq <= 0.0 {
            return None;
        }
        Some(m)
    }

    /// The whole `modules.cpu.powerModel[]` array (cluster order preserved).
    pub fn list_from_modules(modules: &serde_json::Map<String, Value>) -> Vec<Self> {
        modules
            .get("cpu")
            .and_then(|c| c.get("powerModel"))
            .and_then(|p| p.as_array())
            .map(|arr| arr.iter().filter_map(Self::from_value).collect())
            .unwrap_or_default()
    }

    fn ratios(&self) -> (f64, f64) {
        (
            self.plain_freq / self.typical_freq,
            self.sweet_freq / self.typical_freq,
        )
    }

    /// Power for one core at `freq_khz`, in watts.
    pub fn power_at_khz(&self, freq_khz: f64) -> f64 {
        self.typical_power * self.power_ratio(freq_khz)
    }

    /// Dimensionless power ratio (1.0 at `typicalFreq`).
    pub fn power_ratio(&self, freq_khz: f64) -> f64 {
        let x = (freq_khz / 1_000_000.0) / self.typical_freq;
        let (rp, rs) = self.ratios();
        if x < rp {
            rs * rp * x
        } else if x < rs {
            rs * x * x
        } else {
            x * x * x
        }
    }

    /// Energy cost: watts per relative-GHz of performance.
    pub fn cost_at_khz(&self, freq_khz: f64) -> f64 {
        let perf = (self.efficiency / 100.0) * (freq_khz / 1_000_000.0);
        if perf <= 0.0 {
            return f64::INFINITY;
        }
        self.power_at_khz(freq_khz) / perf
    }

    /// Relative performance capacity at `freq_khz` (`efficiency/100 · GHz`).
    pub fn capacity_at_khz(&self, freq_khz: f64) -> f64 {
        (self.efficiency / 100.0) * (freq_khz / 1_000_000.0)
    }

    /// Whole-cluster power (all `nr` cores) at `freq_khz`.
    pub fn cluster_power_at_khz(&self, freq_khz: f64) -> f64 {
        self.power_at_khz(freq_khz) * self.nr as f64
    }

    /// Frequency (kHz) whose capacity equals `cap`, clamped.
    pub fn freq_for_capacity(&self, cap: f64, f_min_khz: f64, f_max_khz: f64) -> f64 {
        let f = (cap / (self.efficiency / 100.0)) * 1_000_000.0;
        f.clamp(f_min_khz, f_max_khz)
    }

    /// Frequency (kHz) whose cost equals `cost`, clamped.
    ///
    /// cost(f) = typicalPower·ratio(f) / ((eff/100)·f). For a fixed region the
    /// ratio is a power law k·x^m, so cost = typicalPower·k·x^m / ((eff/100)·x·f_typ)
    /// ∝ x^(m-1) — solve numerically with a bisection over the OPP range.
    pub fn freq_for_cost(&self, cost: f64, f_min_khz: f64, f_max_khz: f64) -> f64 {
        let (mut lo, mut hi) = (f_min_khz, f_max_khz);
        if cost <= 0.0 || !cost.is_finite() {
            return f_max_khz;
        }
        // cost is increasing in f for this model (cubic numerator beats linear
        // denominator), so a plain bisection converges.
        for _ in 0..48 {
            let mid = 0.5 * (lo + hi);
            if self.cost_at_khz(mid) < cost {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    /// Frequency (kHz) that satisfies `demand` (a power ratio), clamped to
    /// `[f_min_khz, f_max_khz]`.
    ///
    /// Inverts the piecewise curve exactly:
    ///   linear   → x = ratio / (Rs·Rp)
    ///   quadratic→ x = sqrt(ratio / Rs)
    ///   cubic    → x = ratio^(1/3)
    pub fn freq_for_demand(&self, demand: f64, f_min_khz: f64, f_max_khz: f64) -> f64 {
        let (rp, rs) = self.ratios();
        let d = demand.max(0.0);
        // Value of the curve at the two junctions.
        let y_plain = rs * rp * rp;
        let y_sweet = rs * rs * rs;
        let x = if d <= y_plain {
            if rs * rp <= 0.0 {
                0.0
            } else {
                d / (rs * rp)
            }
        } else if d <= y_sweet {
            if rs <= 0.0 {
                0.0
            } else {
                (d / rs).sqrt()
            }
        } else {
            d.cbrt()
        };
        let f = x * self.typical_freq * 1_000_000.0;
        f.clamp(f_min_khz, f_max_khz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// UGT sdm888.json `modules.cpu.powerModel`.
    fn models() -> Vec<PowerModel> {
        let v = json!([
            {"efficiency":115,"nr":4,"typicalPower":0.3,"typicalFreq":1.8,
             "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6},
            {"efficiency":320,"nr":3,"typicalPower":1.6,"typicalFreq":2.4,
             "sweetFreq":1.7,"plainFreq":1.0,"freeFreq":0.7},
            {"efficiency":400,"nr":1,"typicalPower":3,"typicalFreq":2.9,
             "sweetFreq":1.8,"plainFreq":1.6,"freeFreq":0.8}
        ]);
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| PowerModel::from_value(x).unwrap())
            .collect()
    }

    /// The exact `opp <freq> pwr <x> cost <y>` triples the upstream binary
    /// printed on alioth at startup (docs/m5-evidence.md §1).
    const UPSTREAM_OPPS: [&[(f64, f64, f64)]; 3] = [
        &[
            (614400.0, 0.053, 0.075),
            (1171200.0, 0.101, 0.075),
            (1248000.0, 0.112, 0.078),
            (1344000.0, 0.130, 0.084),
            (1420800.0, 0.148, 0.090),
            (1516800.0, 0.180, 0.103),
            (1612800.0, 0.216, 0.116),
            (1708800.0, 0.257, 0.131),
            (1804800.0, 0.302, 0.146),
        ],
        &[
            (710400.0, 0.140, 0.061),
            (1056000.0, 0.219, 0.065),
            (1286400.0, 0.326, 0.079),
            (1478400.0, 0.430, 0.091),
            (1670400.0, 0.549, 0.103),
            (1862400.0, 0.748, 0.125),
            (2054400.0, 1.004, 0.153),
            (2246400.0, 1.312, 0.183),
            (2419200.0, 1.639, 0.212),
        ],
        &[
            (844800.0, 0.299, 0.089),
            (1632000.0, 0.590, 0.090),
            (1747200.0, 0.676, 0.097),
            (2169600.0, 1.256, 0.145),
            (2457600.0, 1.826, 0.186),
            (2841600.0, 2.822, 0.248),
            (3187200.0, 3.982, 0.312),
        ],
    ];

    #[test]
    fn power_and_cost_match_upstream_log_exactly() {
        let ms = models();
        assert_eq!(ms.len(), 3);
        let mut checked = 0;
        for (mi, opps) in UPSTREAM_OPPS.iter().enumerate() {
            let m = &ms[mi];
            for &(freq, want_pwr, want_cost) in opps.iter() {
                let got_pwr = m.power_at_khz(freq);
                let got_cost = m.cost_at_khz(freq);
                assert!(
                    (got_pwr - want_pwr).abs() < 0.0015,
                    "cluster{mi} freq={freq} power {got_pwr:.4} != {want_pwr:.3}"
                );
                assert!(
                    (got_cost - want_cost).abs() < 0.0015,
                    "cluster{mi} freq={freq} cost {got_cost:.4} != {want_cost:.3}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 25, "expected all 25 upstream OPPs");
    }

    #[test]
    fn power_ratio_is_one_at_typical_freq() {
        for m in models() {
            let r = m.power_ratio((m.typical_freq * 1_000_000.0) as f64);
            assert!((r - 1.0).abs() < 0.02, "ratio {r} at typicalFreq");
        }
    }

    #[test]
    fn region_boundaries_are_continuous() {
        // The three pieces must meet at plainFreq and sweetFreq.
        for m in models() {
            let f_plain = m.plain_freq * 1e6;
            let f_sweet = m.sweet_freq * 1e6;
            let (rp, rs) = m.ratios();
            // at plain: linear Rs·Rp·x  vs quadratic Rs·x²
            let a = rs * rp * rp;
            let b = rs * rp * rp;
            assert!((a - b).abs() < 1e-12);
            // at sweet: quadratic Rs·x²  vs cubic x³
            let c = rs * rs * rs;
            let d = rs * rs * rs;
            assert!((c - d).abs() < 1e-12);
            // and the values are finite/monotonic
            assert!(m.power_at_khz(f_plain) < m.power_at_khz(f_sweet));
        }
    }

    #[test]
    fn freq_for_demand_inverts_the_curve() {
        for m in models() {
            for demand in [0.05, 0.15, 0.3, 0.5, 0.85, 1.0, 1.4] {
                let f = m.freq_for_demand(demand, 300_000.0, 3_500_000.0);
                let back = m.power_ratio(f);
                let clamped = f >= 3_500_000.0 || f <= 300_000.0;
                if !clamped {
                    assert!(
                        (back - demand).abs() < 1e-6,
                        "demand {demand} -> {f} kHz -> ratio {back}"
                    );
                }
            }
        }
    }

    #[test]
    fn cluster_power_scales_with_nr() {
        let ms = models();
        let m = &ms[0];
        assert!((m.cluster_power_at_khz(1_804_800.0) - m.power_at_khz(1_804_800.0) * 4.0).abs() < 1e-9);
    }

    #[test]
    fn parses_from_modules_map() {
        let mut modules = serde_json::Map::new();
        modules.insert(
            "cpu".into(),
            json!({"enable": true, "powerModel": [
                {"efficiency":115,"nr":4,"typicalPower":0.3,"typicalFreq":1.8,
                 "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6}
            ]}),
        );
        let list = PowerModel::list_from_modules(&modules);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].efficiency, 115.0);
        assert_eq!(list[0].nr, 4);
    }
}

#[cfg(test)]
mod dbg_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dbg_freq_for_cost() {
        let m = PowerModel::from_value(&json!({
            "efficiency":115,"nr":4,"typicalPower":0.3,"typicalFreq":1.8,
            "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6})).unwrap();
        for target in [0.0751, 0.0782, 0.0909, 0.0967, 0.1283, 0.1457] {
            let f = m.freq_for_cost(target, 614400.0, 1804800.0);
            println!("cost target {target} -> freq {f} (actual cost {:.4})", m.cost_at_khz(f));
        }
    }
}
