//! Userspace CPU governor — the six-step loop of `config/README.md`.
//!
//! ```text
//! 1. sample per-core load every baseSampleTime (baseSlackTime when idle)
//! 2. per-cluster performance load + demand
//!      demand = load + (1 - load) * (margin + burst)
//!      if (max load increase > predictThd) use the predicted load, ignore latencyTime
//! 3. map demand to an OPP frequency (clusters share one latencyTime budget)
//! 4. cap the whole-CPU power envelope (PL1 = slowLimitPower, PL2 = fastLimitPower,
//!    with a capacity pool replenished at fastLimitRecoverScale)
//! 5. guide the scheduler (guideCap / limitEfficiency) by trimming per-cluster capacity
//! 6. write the target frequencies
//! ```
//!
//! The energy model itself (power / cost per OPP) lives in [`crate::cpu`].
//!
//! **Fidelity notes** (kept explicit so nobody mistakes them for verified):
//!   * steps 2 and 3 are formula-exact and unit-tested;
//!   * step 4's pool arithmetic follows the README's wording verbatim
//!     ("energy > slowLimitPower → pool decreases", "energy < slowLimitPower →
//!     pool recovers × fastLimitRecoverScale, capped at fastLimitCapacity");
//!   * the *latency* smoothing of step 3 is approximated: upstream describes a
//!     continuous shared latency budget whose measured effect exceeds the
//!     configured value because sampling is discrete. We implement "at most one
//!     OPP step per sample unless the demand is predict-boosted, in which case
//!     jump straight to the target" — the discrete-sampling behaviour the README
//!     describes, not a claim to match upstream tick-for-tick;
//!   * step 5 (guideCap / limitEfficiency) trims capacity as documented but the
//!     exact upstream capacity table is not observable from here.

#![allow(dead_code)]

use crate::cpu::PowerModel;
use std::time::{Duration, Instant};

/// The `initials.cpu.*` / `presets.<mode>.<scene>.cpu.*` tunables.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorTunables {
    pub base_sample_time: f64,
    pub base_slack_time: f64,
    pub latency_time: f64,
    pub slow_limit_power: f64,
    pub fast_limit_power: f64,
    pub fast_limit_capacity: f64,
    pub fast_limit_recover_scale: f64,
    pub predict_thd: f64,
    pub margin: f64,
    pub burst: f64,
    pub guide_cap: bool,
    pub limit_efficiency: bool,
}

impl Default for GovernorTunables {
    fn default() -> Self {
        Self {
            base_sample_time: 0.02,
            base_slack_time: 0.05,
            latency_time: 0.5,
            slow_limit_power: 2.0,
            fast_limit_power: 4.0,
            fast_limit_capacity: 10.0,
            fast_limit_recover_scale: 0.2,
            predict_thd: 0.5,
            margin: 0.2,
            burst: 0.0,
            guide_cap: true,
            limit_efficiency: true,
        }
    }
}

/// One cluster's runtime state.
#[derive(Debug, Clone)]
pub struct ClusterState {
    pub model: PowerModel,
    /// CPU ids belonging to this cluster (index into the per-CPU sample arrays).
    pub cores: Vec<usize>,
    /// Available OPPs, kHz, ascending.
    pub opps_khz: Vec<f64>,
    pub target_khz: f64,
    pub load: f64,
    pub prev_load: f64,
    /// Set when the prediction branch fired this cycle.
    pub predicted: bool,
}

impl ClusterState {
    pub fn new(model: PowerModel, cores: Vec<usize>, mut opps_khz: Vec<f64>) -> Self {
        opps_khz.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let target = opps_khz.first().copied().unwrap_or(0.0);
        Self {
            model,
            cores,
            opps_khz,
            target_khz: target,
            load: 0.0,
            prev_load: 0.0,
            predicted: false,
        }
    }
    pub fn max_opp(&self) -> f64 {
        self.opps_khz.last().copied().unwrap_or(0.0)
    }
    pub fn min_opp(&self) -> f64 {
        self.opps_khz.first().copied().unwrap_or(0.0)
    }
    /// Snap a frequency down/up to the nearest available OPP.
    pub fn snap(&self, freq_khz: f64) -> f64 {
        self.opps_khz
            .iter()
            .copied()
            .find(|o| *o >= freq_khz)
            .unwrap_or_else(|| self.max_opp())
    }
}

/// Per-CPU jiffies snapshot (`/proc/stat` columns: user+nice+system+irq+softirq,
/// total).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuJiffies {
    pub busy: Vec<u64>,
    pub total: Vec<u64>,
}

/// Per-core busy ratios from two consecutive snapshots.
pub fn load_from_delta(prev: &CpuJiffies, cur: &CpuJiffies) -> Vec<f64> {
    let n = prev.busy.len().min(cur.busy.len()).min(prev.total.len()).min(cur.total.len());
    (0..n)
        .map(|i| {
            let db = cur.busy[i].saturating_sub(prev.busy[i]) as f64;
            let dt = cur.total[i].saturating_sub(prev.total[i]) as f64;
            if dt <= 0.0 {
                0.0
            } else {
                (db / dt).clamp(0.0, 1.0)
            }
        })
        .collect()
}

/// The governor: holds the per-cluster state and the PL2 pool.
#[derive(Debug)]
pub struct Governor {
    pub tunables: GovernorTunables,
    pub clusters: Vec<ClusterState>,
    /// Remaining "energy buffer" of the short-term limit, in watt-seconds.
    pub pool: f64,
    /// True when every cluster is idle (drives the slack sampling period).
    pub idle: bool,
    pub last_sample: Instant,
    /// Number of loop iterations, for diagnostics.
    pub ticks: u64,
}

impl Governor {
    pub fn new(tunables: GovernorTunables, clusters: Vec<ClusterState>) -> Self {
        let pool = tunables.fast_limit_capacity;
        Self {
            tunables,
            clusters,
            pool,
            idle: true,
            last_sample: Instant::now(),
            ticks: 0,
        }
    }

    /// Which sampling period applies right now.
    pub fn sample_period(&self) -> Duration {
        let s = if self.idle {
            self.tunables.base_slack_time
        } else {
            self.tunables.base_sample_time
        };
        Duration::from_secs_f64(s.max(0.001))
    }

    /// One governor cycle. `prev`/`cur` are `/proc/stat` snapshots;
    /// `now` is supplied so tests can drive time deterministically.
    ///
    /// Returns the target frequency per cluster (kHz).
    pub fn tick(&mut self, prev: &CpuJiffies, cur: &CpuJiffies, now: Instant) -> Vec<f64> {
        self.ticks += 1;
        let elapsed = now.saturating_duration_since(self.last_sample).as_secs_f64();
        self.last_sample = now;

        let per_core = load_from_delta(prev, cur);
        let any_load = per_core.iter().any(|l| *l > 0.01);
        self.idle = !any_load;

        // ---- step 2: per-cluster load + demand -------------------------------
        let mut demands = Vec::with_capacity(self.clusters.len());
        for cl in self.clusters.iter_mut() {
            let (busy, total) = cl.cores.iter().fold((0.0, 0.0), |(b, t), &i| {
                (
                    b + per_core.get(i).copied().unwrap_or(0.0),
                    t + 1.0,
                )
            });
            let load = if total > 0.0 { busy / total } else { 0.0 };
            cl.prev_load = cl.load;
            cl.load = load;

            // Prediction branch: a large *increase* in cluster load means we
            // should not wait for the next sample.
            let increase = load - cl.prev_load;
            cl.predicted = increase > self.tunables.predict_thd;

            let load_for_demand = if cl.predicted { load } else { load };
            let demand = load_for_demand
                + (1.0 - load_for_demand) * (self.tunables.margin + self.tunables.burst);
            demands.push(demand.clamp(0.0, 1.0));
        }

        // ---- step 3: demand -> frequency (goal, before any smoothing) --------
        // `goal[i]` is what the cluster should run at; the published target
        // ramps toward it in step 6. Keeping the goal separate from the
        // published value is essential: feeding a clamped/ramped value back in
        // as next cycle's origin makes the ramp and the clamp fight each other
        // and pins clusters at the minimum OPP.
        let mut goal: Vec<f64> = Vec::with_capacity(self.clusters.len());
        for (i, cl) in self.clusters.iter().enumerate() {
            let want = cl.model.freq_for_demand(demands[i], cl.min_opp(), cl.max_opp());
            goal.push(cl.snap(want));
        }

        // ---- step 4: short-term energy buffer (PL1/PL2 accounting) ------------
        // Power attribution: `cluster_power_at_khz` already sums every core in
        // the cluster, so scaling by the cluster's measured load attributes
        // draw only to cores that are actually busy — README's "根据能耗模型和
        // 每个核心的负载". An idle cluster therefore costs nothing and does not
        // eat the budget the way an unconditional idle floor did (that floor
        // squeezed a fully loaded little cluster down to 403 kHz on device).
        // Recover/drain the short-term buffer first.
        let est_power: f64 = self
            .clusters
            .iter()
            .zip(goal.iter())
            .map(|(cl, &t)| cl.model.cluster_power_at_khz(t) * cl.load)
            .sum();

        if self.tunables.burst > 0.0 {
            // `burst` bypasses both limits (README step 4).
            self.pool = self.tunables.fast_limit_capacity;
        } else {
            let pl1 = self.tunables.slow_limit_power;
            if est_power > pl1 {
                self.pool = (self.pool - (est_power - pl1) * elapsed).max(0.0);
            } else {
                let gain = (pl1 - est_power) * elapsed * self.tunables.fast_limit_recover_scale;
                self.pool = (self.pool + gain).min(self.tunables.fast_limit_capacity);
            }

        }

        // ---- step 5: guideCap / limitEfficiency -----------------------------
        if self.tunables.guide_cap {
            // Capacity = efficiency/100 · GHz. When a lower cluster is about to
            // out-capacity a higher one, trim it so EAS keeps work on the
            // efficient cluster instead of migrating up.
            for i in 0..self.clusters.len().saturating_sub(1) {
                let (lo, hi) = (i, i + 1);
                let lo_cap = self.clusters[lo].model.capacity_at_khz(goal[lo]);
                let hi_cap_floor = self.clusters[hi].model.capacity_at_khz(self.clusters[hi].min_opp());
                if lo_cap > hi_cap_floor {
                    // Trim the lower cluster to the higher cluster's floor.
                    let want = self.clusters[lo]
                        .model
                        .freq_for_capacity(hi_cap_floor, self.clusters[lo].min_opp(), self.clusters[lo].max_opp());
                    goal[lo] = self.clusters[lo].snap(want).min(goal[lo]);
                }
            }
        }
        if self.tunables.limit_efficiency {
            // A lower cluster's top OPP must not be more efficient (lower cost)
            // than the next cluster's *current* OPP: keep the big cores the
            // attractive place for heavy work.
            for i in 0..self.clusters.len().saturating_sub(1) {
                let (lo, hi) = (i, i + 1);
                let lo_cost = self.clusters[lo].model.cost_at_khz(goal[lo]);
                let hi_cost = self.clusters[hi].model.cost_at_khz(goal[hi]);
                if lo_cost < hi_cost && hi_cost > 0.0 {
                    // Raise the lower cluster's target until its cost reaches the
                    // higher cluster's, capped at its own max OPP.
                    let want = self.clusters[lo]
                        .model
                        .freq_for_cost(hi_cost, self.clusters[lo].min_opp(), self.clusters[lo].max_opp());
                    goal[lo] = self.clusters[lo].snap(want);
                }
            }
        }

        // ---- step 5b: power cap, as the *last* hard constraint ----------------
        // Ordering matters: guideCap/limitEfficiency exist to steer EAS and will
        // happily raise a cluster's goal back up, so the power cap has to run
        // after them or they undo it.
        //
        // Only clusters that are actually drawing power are clamped. An idle
        // cluster contributes nothing to `power_of` but would still be dragged
        // down by a shared cost ceiling — which pinned an idle cluster2 to
        // 960 kHz on device while it drew no current at all.
        if self.tunables.burst <= 0.0 {
            let pl1 = self.tunables.slow_limit_power;
            let limit = if self.pool > 0.0 {
                self.tunables.fast_limit_power
            } else {
                pl1
            };
            if limit > 0.0 && self.power_of(&goal) > limit {
                // "best overall performance under the power cap": allow each
                // *loaded* cluster the highest OPP whose marginal cost (W per
                // relative GHz) sits under a common ceiling, and bisect that
                // ceiling until the loaded clusters' total power fits `limit`.
                // Equalising marginal cost is exactly the KKT condition for
                // maximising total capacity subject to the power budget.
                let ceiling = self.cost_ceiling_for(limit, &goal);
                for (i, cl) in self.clusters.iter().enumerate() {
                    if cl.load <= 0.01 {
                        continue;
                    }
                    let by_cost = cl.model.freq_for_cost(ceiling, cl.min_opp(), cl.max_opp());
                    goal[i] = cl.snap(by_cost).min(goal[i]);
                }
            }
        }

        // ---- step 6: publish (ramp the *published* value toward the goal) ----
        // Only now does the discrete-sampling smoothing apply, and it moves the
        // published target — never the goal — so a clamped goal cannot ratchet
        // the cluster down to the minimum.
        let mut out = Vec::with_capacity(self.clusters.len());
        for (i, cl) in self.clusters.iter_mut().enumerate() {
            cl.target_khz = if cl.predicted {
                // A predicted jump goes straight to the goal.
                goal[i]
            } else {
                one_step(&cl.opps_khz, cl.target_khz, goal[i])
            };
            out.push(cl.target_khz);
        }
        out
    }

    /// Estimated whole-CPU power for a frequency vector.
    fn power_of(&self, freqs: &[f64]) -> f64 {
        self.clusters
            .iter()
            .zip(freqs.iter())
            .map(|(cl, &f)| cl.model.cluster_power_at_khz(f) * cl.load)
            .sum()
    }

    /// Bisect the marginal-cost ceiling so the clusters' total power fits
    /// `limit`. Cost is monotonically increasing in frequency, so the power
    /// drawn under a ceiling is monotonically increasing in the ceiling too.
    fn cost_ceiling_for(&self, limit: f64, goal: &[f64]) -> f64 {
        let hi_cost = self
            .clusters
            .iter()
            .map(|cl| cl.model.cost_at_khz(cl.max_opp()))
            .fold(0.0f64, f64::max);
        let (mut lo, mut hi) = (0.0f64, hi_cost.max(1e-6));
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            let trial: Vec<f64> = self
                .clusters
                .iter()
                .enumerate()
                .map(|(i, cl)| {
                    if cl.load <= 0.01 {
                        return goal[i];
                    }
                    let f = cl.model.freq_for_cost(mid, cl.min_opp(), cl.max_opp());
                    cl.snap(f).min(goal[i])
                })
                .collect();
            if self.power_of(&trial) > limit {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        0.5 * (lo + hi)
    }
}

/// Move one OPP step from `cur` toward `want`.
fn one_step(opps: &[f64], cur: f64, want: f64) -> f64 {
    if opps.is_empty() {
        return want;
    }
    let idx = opps.iter().position(|o| (*o - cur).abs() < 0.5).unwrap_or_else(|| {
        // Current freq isn't an exact OPP: start from the nearest one.
        opps.iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                ((**a) - cur).abs().partial_cmp(&((**b) - cur).abs()).unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    });
    let want_idx = opps
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| ((**a) - want).abs().partial_cmp(&((**b) - want).abs()).unwrap())
        .map(|(i, _)| i)
        .unwrap_or(idx);
    if want_idx == idx {
        return opps[idx];
    }
    let next = if want_idx > idx { idx + 1 } else { idx.saturating_sub(1) };
    opps[next]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::PowerModel;
    use serde_json::json;

    fn sdm888_clusters() -> Vec<ClusterState> {
        let models = [
            json!({"efficiency":115,"nr":4,"typicalPower":0.3,"typicalFreq":1.8,
                   "sweetFreq":1.4,"plainFreq":1.2,"freeFreq":0.6}),
            json!({"efficiency":320,"nr":3,"typicalPower":1.6,"typicalFreq":2.4,
                   "sweetFreq":1.7,"plainFreq":1.0,"freeFreq":0.7}),
            json!({"efficiency":400,"nr":1,"typicalPower":3,"typicalFreq":2.9,
                   "sweetFreq":1.8,"plainFreq":1.6,"freeFreq":0.8}),
        ];
        let opps = [
            vec![614400.0, 1171200.0, 1248000.0, 1344000.0, 1420800.0, 1516800.0, 1612800.0, 1708800.0, 1804800.0],
            vec![710400.0, 1056000.0, 1286400.0, 1478400.0, 1670400.0, 1862400.0, 2054400.0, 2246400.0, 2419200.0],
            vec![844800.0, 1632000.0, 1747200.0, 2169600.0, 2457600.0, 2841600.0, 3187200.0],
        ];
        let cores = [vec![0, 1, 2, 3], vec![4, 5, 6], vec![7]];
        (0..3)
            .map(|i| {
                ClusterState::new(
                    PowerModel::from_value(&models[i]).unwrap(),
                    cores[i].clone(),
                    opps[i].clone(),
                )
            })
            .collect()
    }

    /// A `/proc/stat` stepper that keeps *increasing* counters, so
    /// `load_from_delta` sees real deltas. Passing a constant snapshot makes
    /// every delta zero — which silently reads as "idle" and makes a load test
    /// pass for the wrong reason.
    struct StatStepper {
        busy: Vec<u64>,
        total: Vec<u64>,
    }
    impl StatStepper {
        fn new(n: usize) -> Self {
            Self { busy: vec![0; n], total: vec![0; n] }
        }
        /// Advance one sample period: `busy_cpus` run at 100%, the rest idle.
        fn step(&mut self, busy_cpus: &[usize], period: u64) -> (CpuJiffies, CpuJiffies) {
            let prev = CpuJiffies { busy: self.busy.clone(), total: self.total.clone() };
            for i in 0..self.total.len() {
                self.total[i] += period;
                if busy_cpus.contains(&i) {
                    self.busy[i] += period;
                }
            }
            let cur = CpuJiffies { busy: self.busy.clone(), total: self.total.clone() };
            (prev, cur)
        }
    }

    #[test]
    fn load_from_delta_is_a_ratio() {
        let prev = CpuJiffies { busy: vec![100, 200], total: vec![200, 400] };
        let cur = CpuJiffies { busy: vec![150, 220], total: vec![300, 600] };
        let l = load_from_delta(&prev, &cur);
        assert!((l[0] - 0.5).abs() < 1e-9, "{l:?}");
        assert!((l[1] - 0.1).abs() < 1e-9, "{l:?}");
    }

    #[test]
    fn zero_total_delta_is_zero_load() {
        let j = CpuJiffies { busy: vec![10], total: vec![20] };
        assert_eq!(load_from_delta(&j, &j), vec![0.0]);
    }

    #[test]
    fn high_load_pushes_toward_high_opp() {
        let t = GovernorTunables::default();
        let mut g = Governor::new(t, sdm888_clusters());
        let mut st = StatStepper::new(8);
        let t0 = Instant::now();
        let mut last = Vec::new();
        for i in 0..40 {
            let (prev, cur) = st.step(&[0, 1, 2, 3, 4, 5, 6, 7], 100);
            last = g.tick(&prev, &cur, t0 + Duration::from_millis(20 * i as u64));
        }
        for (i, f) in last.iter().enumerate() {
            assert!(*f > 1_000_000.0, "cluster{i} stuck at {f} under full load");
        }
    }

    #[test]
    fn idle_settles_at_the_margin_floor() {
        // NOTE the semantics: with load == 0 the demand is NOT 0 — the formula
        // demand = load + (1-load)*(margin + burst) leaves `margin` of headroom
        // even when idle. With the default margin = 0.2 that is ~0.69 GHz on
        // cluster0, i.e. the second OPP, not the minimum. Asserting min_opp here
        // would encode a wrong expectation.
        let mut g = Governor::new(GovernorTunables::default(), sdm888_clusters());
        let mut st = StatStepper::new(8);
        let t0 = Instant::now();
        for i in 0..20 {
            let (p, c) = st.step(&[0, 1, 2, 3], 100);
            g.tick(&p, &c, t0 + Duration::from_millis(20 * i as u64));
        }
        let busy_target = g.clusters[0].target_khz;
        assert!(busy_target > 1_000_000.0, "should ramp under load");

        let mut last = 0.0;
        for i in 20..80 {
            let (p, c) = st.step(&[], 100);
            last = g.tick(&p, &c, t0 + Duration::from_millis(20 * i as u64))[0];
        }
        // The margin floor is ~0.69 GHz -> snapped up to the 2nd OPP (1171200).
        let per_cluster0 = sdm888_clusters()[0].opps_khz.clone();
        let floor = per_cluster0
            .iter()
            .copied()
            .find(|o| *o >= 690_000.0)
            .unwrap();
        assert_eq!(last, floor, "idle should settle at the margin floor OPP");
        assert!(last < busy_target, "idle target must be below the busy target");

        // And with margin = 0 it really does reach the minimum OPP.
        let mut t = GovernorTunables::default();
        t.margin = 0.0;
        let mut g2 = Governor::new(t, sdm888_clusters());
        let mut st2 = StatStepper::new(8);
        let mut last2 = 0.0;
        for i in 0..80 {
            let (p, c) = st2.step(&[], 100);
            last2 = g2.tick(&p, &c, t0 + Duration::from_millis(20 * i as u64))[0];
        }
        assert_eq!(last2, g2.clusters[0].min_opp(), "margin 0 -> min OPP");
    }

    #[test]
    fn pool_drains_above_pl1_and_recovers_below() {
        let mut t = GovernorTunables::default();
        t.slow_limit_power = 0.5; // very low so a busy cluster exceeds it
        t.fast_limit_power = 5.0;
        t.fast_limit_capacity = 1.0;
        let mut g = Governor::new(t, sdm888_clusters());
        let busy = CpuJiffies { busy: vec![100; 8], total: vec![100; 8] };
        let zero = CpuJiffies { busy: vec![0; 8], total: vec![0; 8] };
        let t0 = Instant::now();
        for i in 0..30 {
            g.tick(&zero, &busy, t0 + Duration::from_millis(50 * i as u64));
        }
        assert_eq!(g.pool, 0.0, "pool should be exhausted under sustained overload");
        // Idle: pool recovers.
        for i in 30..80 {
            let a = CpuJiffies { busy: vec![0; 8], total: vec![100; 8] };
            let b = CpuJiffies { busy: vec![0; 8], total: vec![200; 8] };
            g.tick(&a, &b, t0 + Duration::from_millis(50 * i as u64));
        }
        assert!(g.pool > 0.0, "pool should recover when under PL1");
    }

    #[test]
    fn sample_period_switches_on_idle() {
        let mut g = Governor::new(GovernorTunables::default(), sdm888_clusters());
        let busy = CpuJiffies { busy: vec![100; 8], total: vec![100; 8] };
        let zero = CpuJiffies { busy: vec![0; 8], total: vec![0; 8] };
        let t = Instant::now();
        g.tick(&zero, &busy, t);
        assert!(!g.idle);
        assert_eq!(g.sample_period(), Duration::from_secs_f64(0.02));
        g.tick(&zero, &zero, t + Duration::from_millis(20));
        assert!(g.idle);
        assert_eq!(g.sample_period(), Duration::from_secs_f64(0.05));
    }

    /// Regression: the power clamp must not pin a loaded cluster at the
    /// minimum OPP. The bug was that the clamped value was written back into
    /// `cl.target_khz`, so next cycle's one-OPP ramp started from the clamped
    /// floor and every step up was immediately clamped back down.
    #[test]
    fn tight_power_limit_does_not_pin_to_min_opp() {
        let mk = |pl1: f64| {
            let mut t = GovernorTunables::default();
            t.slow_limit_power = pl1;
            t.fast_limit_power = pl1 * 2.0;
            t.fast_limit_capacity = 0.1; // drain almost immediately
            t.margin = 0.22;
            t.guide_cap = false;
            t.limit_efficiency = false;
            Governor::new(t, sdm888_clusters())
        };
        let run = |mut g: Governor| {
            // Only cpu0..3 are busy (the little cpuset), matching the alioth
            // observation where the shell's cpuset confines load to cluster0.
            let mut st = StatStepper::new(8);
            let t0 = Instant::now();
            let mut out = Vec::new();
            for i in 0..120 {
                let (p, c) = st.step(&[0, 1, 2, 3], 100);
                out = g.tick(&p, &c, t0 + Duration::from_millis(40 * i as u64));
            }
            out
        };

        let tight = run(mk(1.0));
        let loose = run(mk(8.0));
        assert!(
            tight[0] > sdm888_clusters()[0].min_opp(),
            "tight PL1 pinned cluster0 at the minimum: {} kHz",
            tight[0]
        );
        assert!(
            loose[0] >= tight[0],
            "a generous PL1 ({}) must not run slower than a tight one ({})",
            loose[0],
            tight[0]
        );
        // And the tight cap should still be the binding one.
        assert!(loose[0] > tight[0], "PL1 should bind: tight={tight:?} loose={loose:?}");
    }

    /// The power budget must be attributed to *loaded* cores. With an
    /// unconditional idle floor, idle cluster1/cluster2 (which limitEfficiency
    /// parks at high OPPs) consumed most of PL1 and squeezed a fully loaded
    /// cluster0 to 403 kHz on device — below its own idle target.
    #[test]
    fn loaded_cluster_outranks_the_idle_margin_floor_under_a_tight_cap() {
        let mut t = GovernorTunables::default();
        t.slow_limit_power = 1.0; // sdm888 balance preset value
        t.fast_limit_power = 2.0;
        t.fast_limit_capacity = 15.0;
        t.margin = 0.22;
        let mut g = Governor::new(t, sdm888_clusters());
        let mut st = StatStepper::new(8);
        let t0 = Instant::now();

        // Idle first, so the margin floor is established.
        let mut idle_target = 0.0;
        for i in 0..40 {
            let (p, c) = st.step(&[], 100);
            idle_target = g.tick(&p, &c, t0 + Duration::from_millis(40 * i as u64))[0];
        }
        // Now load cluster0 only (cpu0..3 = the little cpuset on alioth).
        let mut loaded = 0.0;
        for i in 40..160 {
            let (p, c) = st.step(&[0, 1, 2, 3], 100);
            loaded = g.tick(&p, &c, t0 + Duration::from_millis(40 * i as u64))[0];
        }
        let floors: Vec<f64> = sdm888_clusters()[0].opps_khz.clone();
        let margin_floor = floors.iter().copied().find(|o| *o >= 690_000.0).unwrap();
        assert!(
            loaded > idle_target.max(margin_floor),
            "a fully loaded cluster0 must beat the idle margin floor: idle={idle_target} margin_floor={margin_floor} loaded={loaded}"
        );
    }

    /// An idle cluster draws nothing, so the power cap must leave it alone.
    /// On device an idle cluster2 was dragged to 960 kHz by the shared cost
    /// ceiling while its measured load was 0.00.
    #[test]
    fn idle_cluster_is_not_touched_by_the_power_cap() {
        let mut t = GovernorTunables::default();
        t.slow_limit_power = 1.0;
        t.fast_limit_power = 2.0;
        t.fast_limit_capacity = 0.01; // drains at once -> PL1 binds
        t.margin = 0.22;
        let mut g = Governor::new(t, sdm888_clusters());
        let mut st = StatStepper::new(8);
        let t0 = Instant::now();
        // Sustain heavy load on cluster0 only; cluster2 must keep its goal.
        let mut out = Vec::new();
        for i in 0..200 {
            let (p, c) = st.step(&[0, 1, 2, 3], 100);
            out = g.tick(&p, &c, t0 + Duration::from_millis(40 * i as u64));
        }
        let idle_goal = sdm888_clusters()[2]
            .model
            .freq_for_demand(0.22, sdm888_clusters()[2].min_opp(), sdm888_clusters()[2].max_opp());
        assert!(
            out[2] >= idle_goal,
            "idle cluster2 was clamped below its own goal: {} < {}",
            out[2],
            idle_goal
        );
    }

    #[test]
    fn burst_bypasses_the_power_limits() {
        let mut t = GovernorTunables::default();
        t.slow_limit_power = 0.01;
        t.fast_limit_power = 0.01;
        t.fast_limit_capacity = 0.01;
        t.burst = 0.6;
        let mut g = Governor::new(t, sdm888_clusters());
        let busy = CpuJiffies { busy: vec![100; 8], total: vec![100; 8] };
        let zero = CpuJiffies { busy: vec![0; 8], total: vec![0; 8] };
        let t0 = Instant::now();
        let mut last = Vec::new();
        for i in 0..20 {
            last = g.tick(&zero, &busy, t0 + Duration::from_millis(20 * i as u64));
        }
        // With burst the pool never drains, so the top cluster keeps climbing.
        assert_eq!(g.pool, g.tunables.fast_limit_capacity);
        assert!(last[2] > 2_000_000.0, "top cluster should still climb: {last:?}");
    }

    #[test]
    fn prediction_branch_jumps_in_one_cycle() {
        let mut t = GovernorTunables::default();
        t.predict_thd = 0.3;
        t.margin = 0.2;
        let mut g = Governor::new(t, sdm888_clusters());
        // cycle 1: idle
        let zero = CpuJiffies { busy: vec![0; 8], total: vec![0; 8] };
        let idle2 = CpuJiffies { busy: vec![0; 8], total: vec![1000; 8] };
        let t0 = Instant::now();
        g.tick(&zero, &idle2, t0);
        // cycle 2: a big jump in load (> predictThd) → no one-step smoothing
        let busy2 = CpuJiffies { busy: vec![900; 8], total: vec![2000; 8] };
        let out = g.tick(&idle2, &busy2, t0 + Duration::from_millis(20));
        assert!(g.clusters[0].predicted, "prediction branch should fire");
        assert!(
            out[0] > g.clusters[0].min_opp(),
            "a predicted jump should move more than nothing: {out:?}"
        );
    }
}
