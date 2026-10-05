# M6 — context scheduler (`modules.sched`)

Status legend: **[V]** verified by tool output · **[I]** inferred · **[U]** unknown.

## 1. Schema, taken from the configs rather than the prose [V]

All 63 UGT configs (and the 38 upstream ones) carry an identical shape:

```jsonc
"sched": {
  "enable": true,
  "cpumask":  { "all": [0..7], "c0": [0,1,2,3], "c1": [4,5,6], "c2": [7] },
  "affinity": { "ui": { "bg": "", "fg": "all", "idle": "all", "touch": "c1", "boost": "all" }, ... },
  "prio":     { "ui": { "bg": -3, "fg": 120, "idle": 110, "touch": 98, "boost": 116 }, ... },
  "rules": [ { "name": "Launcher", "regex": "/HOME_PACKAGE/", "pinned": true,
               "rules": [ { "k": "/MAIN_THREAD/", "ac": "crit", "pc": "rtusr" }, ... ] } ]
}
```

Census over the whole tree: 101 sched modules, 1014 process rules, 4048 patterns,
**28 unique patterns**. Classes are `auto/norm/bg/ui/crit/gtcoop/gtmain` (+`fuck`);
scenes are `bg/fg/idle/touch/boost`.

`prio` codes decode exactly as `config/README.md` lines 217-224 state, and every
value used by a shipped config sits in a documented band:
`0` skip · `1..98` `SCHED_FIFO` · `100..139` `SCHED_NORMAL` (nice = code − 120) ·
`-1` NORMAL · `-2` BATCH · `-3` IDLE.

`pinned` is **not** first-match-wins: README line 248 defines it as "始终作为
`处于顶层可见的进程`应用规则" — the rule is always evaluated as if the process were
the top-visible one, so its scene comes from the hint FSM instead of bg/fg
detection. Implemented that way in `SchedPlanner::scene_for`.

## 2. PCRE2 is NOT required — the AGENT.md assumption was wrong [V]

AGENT.md §3.2 asserted "PCRE2 必须：Rust `regex` crate 不支持现有配置语义". The data
says otherwise. Every one of the 28 unique shipped patterns is plain ERE:

* no lookahead / lookbehind (`(?=` `(?!` `(?<=` `(?<!`),
* no atomic groups `(?>`, no branch reset `(?|`, no conditionals `(?(`, no recursion `(?R`,
* no `\K`, no inline flags `(?i)` `(?m)`, no numbered backreferences.

`uperf-config/tests/all_sched_configs.rs::no_shipped_pattern_needs_pcre2` walks both
config trees and fails if any of those constructs ever appears, and
`every_shipped_config_compiles_and_only_the_known_defects_remain` compiles all 4048
occurrences with the `regex` crate. Using a pure-Rust engine also keeps the Android
staticlib free of a PCRE2 C dependency.

## 3. Two real defects in the shipped configs (tolerated, whitelisted) [V]

The acceptance gate initially failed on 15 findings, all genuine:

* `sdm8g1+.json` is the reference config and defines `affinity.fuck` + `prio.fuck`;
  **`sdm8g2.json` / `sdm8g3.json` define `prio.fuck` but forgot `affinity.fuck`**;
  **`sdm7g1.json` defines neither** — yet all three reference
  `"ac": "fuck", "pc": "fuck"` in the same bloatware rule.
* `sdm8g3.json` uses `"c1,c2"` as an affinity value (8 occurrences in the tree) — a
  **comma-separated list of cpumask group names**, which must be unioned. `c1=[2,3]`
  ∪ `c2=[4,5,6]` = `[2,3,4,5,6]`.

Upstream clearly tolerates both (these configs shipped), so the planner is tolerant
too: a dangling class is a **no-op for that aspect**, an unknown cpumask name makes
the whole mask unusable, and each is recorded as an `Anomaly` instead of a fatal
error. The gate then asserts the anomaly set equals exactly

```rust
[("sdm7g1.json", 2), ("sdm8g2.json", 1), ("sdm8g3.json", 1)]
```

so a *new* defect fails the build while the known three stay documented.

## 4. Cross-layer bug: the supervisor's SIGCHLD handler reaps our children [V]

`/system/bin/cmd` was unreachable from the worker:

```text
Rust: cannot resolve the home package (cmd: spawn failed: No child processes (os error 10))
```

`os error 10` is `ECHILD`. The vendored dfps daemon installs
`signal(SIGCHLD, DaemonSigHandler)` (to notice a worker dying) and that handler calls
`wait(2)`. The worker is forked *after* the handler is installed, so it inherits it,
and the inherited handler reaped the child that Rust's `Command::output()` was
waiting for. **Any `std::process::Command` in the worker fails with ECHILD.**

The worker supervises nothing, so `uperf_rs_start` now resets the disposition:

```rust
unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL); }
```

After the fix the device logs `Rust: current home is 'com.android.launcher3'` —
byte-identical to the upstream log line, which is also the confirmation that
`/HOME_PACKAGE/` means the launcher package resolved from the resolved HOME activity.

## 5. A failure mode that would have taken the device down [V]

The first device dry run printed:

```text
Rust: current home is ''
Rust: sched[DRY] pid=1 "/system/bin/init" ... rule="Launcher" ... -> cpus=Some([0..7]) policy=Fifo(97)
```

A failed resolution produced an **empty package**, so `/HOME_PACKAGE/` was replaced by
`""`, the launcher rule's regex became the empty pattern, and an empty pattern
**matches every process** — 3400 decisions, all of them `FIFO 97` on every thread of
every process on the system. Three guards now exist:

1. `resolve_home_package_with` treats an empty/whitespace result as a *failure*
   (the first version returned it as a valid package);
2. on failure the literal `/HOME_PACKAGE/` is kept, which matches no real process;
3. `SchedPlanner::new` rejects an **empty process pattern** outright
   (`SchedError::EmptyProcessPattern`) — the shipped configs use the literal `"."`
   when they mean "everything", so an empty pattern is never intended.

## 6. Host-level finding: unprivileged priority is asymmetric [V]

```text
->IDLE    ok
->NORMAL  EPERM
->BATCH   EPERM
->FIFO10  EPERM
```

With `CapEff = 0`, a thread may **lower** its own scheduling class but not raise it
back. The unit tests are written to accept either outcome and assert only what the
kernel permits; on device uperf runs as real root where the round trip succeeds.

Also: `/proc/<tid>/stat` field **18 is `priority` (= 20 + nice)**; nice is field
**19**. Reading index 15 returns e.g. 25 for a nice of 5 — which is how the off-by-one
announced itself, and there is now a regression test asserting the relationship.

## 7. Device verification, and the control experiment that saved it [V]

### Semantic check (dry run, shipped config)

`UPERF_SCHED_DRY_RUN=1`, `config/sdm888.json`, scene `idle`, top app unknown:

```text
Rust: current home is 'com.android.launcher3'
Rust: sched[DRY] pid=1384 "/system/bin/surfaceflinger" tid=1384 "surfaceflinger"   rule="SurfaceFlinger" scene=idle ac=crit pc=auto -> cpus=Some([0..7]) policy=Skip
Rust: sched[DRY] pid=1384 "/system/bin/surfaceflinger" tid=1421 "binder:1384_1"    rule="SurfaceFlinger" scene=idle ac=bg   pc=auto -> cpus=Some([0,1,2,3]) policy=Skip
Rust: sched[DRY] pid=1046 "zygote64" tid=1046 "main"                               rule="Default rule"   scene=bg   ac=ui   pc=ui   -> cpus=None policy=Idle

distinct rules: 1761 Default rule, 205 MediaProvider, 95 SurfaceFlinger, 40 SystemUI, 25 SystemServer
```

That matches the config's intent: the launcher/surfaceflinger/system_server rules
capture their processes (binder threads pushed to the little cluster `c0`, main and
render threads to `all`), and everything else falls to the `"regex": "."` Default rule
in the `bg` scene, which is a no-op for affinity (`affinity[*][bg]` is empty for every
class) and `prio[ui][bg] = -3` = `SCHED_IDLE`.

### The first "live success" was a false positive

A live run against probe processes appeared to work: the probes read back
`cpus=0-3` and `policy=5` — exactly the shipped config's values. A **control run with
no uperf at all** reproduced both within 2 seconds:

```text
uperf running right now? -> 0 process(es)
t+0s  A: cpus=Cpus_allowed_list:0-7 policy=0
t+2s  A: cpus=Cpus_allowed_list:0-3 policy=0
t+2s  B: cpus=Cpus_allowed_list:0-7 policy=5
```

Android's own task-profile / cpuset controller reclassifies newly-spawned background
processes on a ~1-2 s timescale, and on this device that lands on the same values the
config asks for. **Any live scheduler test must therefore use target values disjoint
from Android's own**, or it proves nothing. (It also explains why a probe pinned to
cpu 7 read back `0-7`: `taskset -p 80` on the same process returns `Invalid argument`
— cpu 7 is outside that probe's cpuset.)

### Isolation experiment (disjoint values)

A test-only config assigning `cpus=[0,1]` and `SCHED_BATCH` (policy 3) — Android's
controller produces 0-3 / policy 5, so the two are disjoint:

```text
BEFORE(pid=24645, after Android settled): cpus=Cpus_allowed_list:0-7 policy=5
t+1s : cpus=Cpus_allowed_list:0-1 policy=3
t+2s : cpus=Cpus_allowed_list:0-1 policy=3
t+3s : cpus=Cpus_allowed_list:0-1 policy=3
t+6s : cpus=Cpus_allowed_list:0-1 policy=3

Rust: sched pid=24645 ".../sched_probe_iso/probe" tid=24645 "probe" rule="Probe" scene=bg
      ac=probe pc=probe -> cpus=Some([0, 1]) policy=Batch
Rust: sched scene=idle top=None procs=1/1 threads=1 aff=1 (0 ineffective) prio=1 err=0
```

Both changes are exactly what the config asked for and stable for 6 s, with the
post-write verification reporting `0 ineffective`. That is the end-to-end proof:
config → rule match → scene → (`ac`,`pc`) → `sched_setaffinity` + `sched_setscheduler`.

Because a write can be accepted and still lose to an external controller, the applier
now **reads `Cpus_allowed_list` back** and counts `affinity_ineffective` (logging the
discrepancy) instead of trusting the syscall's return code.

### Process-name notes

* the process regex matches `/proc/<pid>/cmdline`'s first field, which is why the
  config's `/system/bin/surfaceflinger` rule works;
* `comm` is truncated to **15 characters** (`TASK_COMM_LEN`), so
  `com.android.launcher3` appears as `com.android.lau`. `/MAIN_THREAD/` is substituted
  with that observed `comm`, which makes the substitution self-consistent (the pattern
  and the thread name come from the same truncated string) — the Launcher probe
  matched `/MAIN_THREAD/` and got `ac=crit pc=rtusr` as a result.

## 8. Reproduce

```sh
cd ~/uperf-rewrite && export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make check          # 115 host assertions + device build
cd rust && cargo test --release

# device, semantics only (changes nothing):
adb push build/aarch64-linux-android23/runnable/uperf /data/local/tmp/sched_uperf
adb shell su -c 'UPERF_SCHED_DRY_RUN=1 /data/local/tmp/sched_uperf <cfg> -o <log>'
grep 'sched\[DRY\]' <log> | head

# device, live, confined to probe processes (UPERF_SCHED_ONLY):
UPERF_SCHED_ONLY=<substr> /data/local/tmp/sched_uperf <test-cfg> -o <log>
```

Two harness-only environment variables, explicitly not config semantics:
`UPERF_SCHED_DRY_RUN=1` (log decisions, touch nothing) and `UPERF_SCHED_ONLY=<substr>`
(restrict to processes whose name contains the substring). `UPERF_HOME_PACKAGE`
overrides `/HOME_PACKAGE/` resolution.

## 9. Still open

* **The `pinned` + `fg`/`bg` interaction with the real top-app signal is only tested
  in dry run.** The live proof used a synthetic config; a live run against the shipped
  config requires knowing the top app, which needs the `topapp.pkgName` event path
  exercised with the launcher actually in the foreground. **[I]**
* One observation is unexplained: in an earlier isolation run requesting `[7]` (a CPU
  outside that probe's cpuset) the syscall's return code reported success while the
  effective mask stayed `0-7`. The post-write verification exists precisely to surface
  that class of case; the underlying cause was not isolated further. **[U]**
* `modules.sched` scene changes do not yet trigger an immediate rescan of *untouched*
  processes — the scan runs on a generation change or every ~1 s. Upstream's timing is
  unknown. **[I]**
* `anim` / `log` / `atrace` modules and `cur_powermode.txt` hot reload are not
  implemented. **[U]**
