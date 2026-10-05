//! `/proc/stat` sampler for the governor (device side).
//!
//! Parses the per-CPU lines (`cpu0 …`) into [`CpuJiffies`]. The `user nice
//! system irq softirq` columns are the busy time; the rest (idle + iowait) is
//! the non-busy part. `guest`/`guest_nice` are already counted inside `user` /
//! `nice` by the kernel, so they must NOT be added again.

#![allow(dead_code)]

use crate::governor::CpuJiffies;

/// Parse a `/proc/stat` body. `n_cpus` caps how many CPUs we keep (the config's
/// cluster layout decides which indices we care about).
pub fn parse_stat(text: &str, n_cpus: usize) -> CpuJiffies {
    let mut busy = vec![0u64; n_cpus];
    let mut total = vec![0u64; n_cpus];
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("cpu") else { continue };
        let mut it = rest.split_whitespace();
        let Some(idx) = it.next() else { continue };
        let Ok(id) = idx.parse::<usize>() else { continue };
        if id >= n_cpus {
            continue;
        }
        let nums: Vec<u64> = it.filter_map(|t| t.parse::<u64>().ok()).collect();
        if nums.len() < 5 {
            continue;
        }
        // user nice system idle iowait irq softirq [steal guest guest_nice]
        let busy_sum = nums[0] + nums[1] + nums[2] + nums[5] + nums.get(6).copied().unwrap_or(0);
        let all: u64 = nums.iter().take(8).sum(); // through steal
        busy[id] = busy_sum;
        total[id] = all;
    }
    CpuJiffies { busy, total }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
cpu  100 0 50 800 10 0 5 0 0 0
cpu0 10 0 5 80 1 0 1 0 0 0
cpu1 20 0 10 160 2 0 1 0 0 0
cpu2 30 0 15 240 3 0 2 0 0 0
cpu3 40 0 20 320 4 0 1 0 0 0
intr 12345 0 0 0
ctxt 999
";

    #[test]
    fn parses_per_cpu_rows() {
        let j = parse_stat(SAMPLE, 4);
        // cpu0: busy = 10+0+5+0+1 = 16 ; total = 10+0+5+80+1+0+1+0 = 97
        assert_eq!(j.busy[0], 16);
        assert_eq!(j.total[0], 97);
        // cpu3: busy = 40+0+20+0+1 = 61 ; total = 40+0+20+320+4+0+1 = 385
        assert_eq!(j.busy[3], 61);
        assert_eq!(j.total[3], 385);
    }

    #[test]
    fn ignores_the_aggregate_line_and_non_cpu_lines() {
        let j = parse_stat(SAMPLE, 4);
        // The aggregate "cpu " row must not leak into cpu0.
        assert_ne!(j.busy[0], 100);
        assert_eq!(j.busy.len(), 4);
    }

    #[test]
    fn caps_at_n_cpus() {
        let j = parse_stat(SAMPLE, 2);
        assert_eq!(j.busy.len(), 2);
        assert_eq!(j.total.len(), 2);
    }

    #[test]
    fn guest_columns_are_not_double_counted() {
        // user=100 already includes guest=50; adding guest again would give 150.
        let s = "cpu0 100 0 0 0 0 0 0 0 50 0\n";
        let j = parse_stat(s, 1);
        assert_eq!(j.busy[0], 100);
        assert_eq!(j.total[0], 100);
    }

    #[test]
    fn delta_between_two_snapshots_is_the_load() {
        let a = parse_stat("cpu0 100 0 0 100 0 0 0 0 0 0\n", 1);
        let b = parse_stat("cpu0 150 0 0 200 0 0 0 0 0 0\n", 1);
        let l = crate::governor::load_from_delta(&a, &b);
        // busy +50, total +150 -> 1/3
        assert!((l[0] - 1.0 / 3.0).abs() < 1e-9, "{l:?}");
    }
}
