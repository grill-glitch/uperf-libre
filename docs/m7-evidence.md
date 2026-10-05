# M7 — packaging, install on device, and the two bugs that made the phone lag

Status legend: **[V]** verified by tool output · **[I]** inferred · **[U]** unknown.

## 1. The lag, diagnosed by measurement [V]

The phone felt unusable while the daemon was running. Two independent causes, both
found by measuring rather than guessing, and both now fixed.

### 1.1 The watcher spun a core against the daemon's own log

`uperf-watch` had the highest cumulative CPU of any thread in the process
(1814 ticks) while every vendored C++ module sat at ~0. The cause was a feedback
loop: the watcher arms a watch on the **parent directory** of the preset files
(needed to catch a rename-into-place), and that mask included `IN_CLOSE_WRITE` and
`IN_MODIFY` — while the daemon's log lives in that same directory
(`USER_PATH/uperf_log.txt` sits beside `cur_powermode.txt`). Every log write woke
the watcher, which re-read its files and logged again.

Measured with `CLK_TCK=100` (a busy loop for 10 s = 999 ticks, so 100 ticks = 1 core
second):

| | worker CPU over 10 s |
|---|---|
| **before**, log inside the watched directory | **1065 ticks (~1.07 cores)** |
| log outside the watched directory | 61 ticks |
| **after** restricting the directory watch to structural events, log back inside | **68 ticks** |

A 15.7x difference, and in production the log is *always* inside that directory, so
this was permanent: a core burning plus continuous storage writes from boot. Fixed
by making the directory watch `IN_CREATE | IN_MOVED_TO | IN_DELETE` only — content
writes are what the per-file watches are for. There is now a regression test
(`sibling_file_writes_do_not_wake_us`) whose comment records the shape.

### 1.2 The governor takeover left the CPU at the wrong frequency

The userspace takeover computes a `demand`-based target and caps it with the config's
PL1. Against the platform's own governor, same workloads, same device:

| | idle | under 4 busy threads |
|---|---|---|
| stock `schedutil` policy0 | 1804800 (max) | 1804800 |
| ours policy0 | 883200–1420800 | **691200 (min)** |

`config/sdm888.json` caps the whole CPU at PL1 = 1.0 W and allocates it by marginal
cost; the little cluster (efficiency 115 vs 320/400) gets almost nothing, so
interactive work that runs there drops to the minimum OPP. That is faithful to
"maximum capacity per watt" and terrible for how a phone feels.

Worse, a policy could be left in `userspace` after a stop (observed twice), which
pins it at the last published frequency with nothing able to fix it except a manual
write or a reboot. And the script-side safety net I added in M6b **invented
`powersave`** when it had no recorded original — on alioth the real boot default is
`schedutil`, so that "restore" put the CPU at its minimum permanently. That is very
likely what the user felt.

Decisions, all evidenced:

* **The takeover is off unless `UPERF_CPU_GOVERNOR=1`.** Upstream's own governor
  cannot run on this kernel at all (all eight `CpufreqWriter` strategies need a
  min-freq knob the driver locks), so taking over preserves no upstream behaviour —
  it is a new device-level policy, and the default should not be one that measured
  worse than stock.
* The daemon **records the governors it replaces** to `USER_PATH/orig_governor.txt`
  (same path and format the scripts use), so a restorer has the truth.
* The script **never invents a governor**: with no recorded original it leaves the
  policy alone and says so.
* `uperf_start` first undoes a stale takeover (a previous crash), then records the
  originals.

Verified after the fix: opt-in arms all three policies and writes
`policy0 schedutil / policy4 schedutil / policy7 schedutil`; stop restores
`schedutil` with zero processes left; a forced `userspace` with no daemon gets
corrected on the next start.

## 2. Packaging and install on alioth (KernelSU) [V]

`build.sh pack` stages the module tree, **overwrites `bin/uperf` with the freshly
built executable**, copies LICENSE/NOTICE, and also refreshes the in-tree
`magisk/bin/uperf` so the repository never carries the closed-source binary.
`build.sh check` now asserts `magisk/bin/uperf` is **this** build, which covers both
failure modes at once — upstream's dev-22.09.04 binary still sitting there, or a
stale copy. Demonstrated by the guard firing on the in-tree upstream binary:

```text
 !! magisk/bin/uperf differs from the built binary
    module: 1461512 bytes      <- upstream
    build : 2227648 bytes
    fix   : sh build.sh Release pack
```

Install path (alioth is KernelSU Next, not Magisk — `magisk --install-module` from
the scripts does not exist there):

```sh
ksud module install /data/local/tmp/uperf-magisk.zip   # stages to modules_update/uperf
# reboot -> KernelSU activates it as modules/uperf
```

Checked before rebooting: the staged binary's md5 equals the local build's exactly
(`f5ff1ce0520fd402121e544c12c8db82`), `setup.sh` ran (its `config/` removal is
visible), and the SoC mapping picked the right config — `ro.board.platform=kona`
maps to `sdm865` in `libsysinfo.sh`, and the seeded `uperf.json` reports
`"name": "sdm865/sdm865+/sdm870[22.09.04]"`.

## 3. Post-boot acceptance [V]

| §1 criterion 1 | result |
|---|---|
| module active | `/data/adb/modules/uperf/`, `modules_update/` empty |
| started at boot | 2 `uperf` processes, from `service.sh` → `script/initsvc.sh` |
| log at the contract path | `/sdcard/Android/yc/uperf/uperf_log.txt`, 1075 lines |
| **no `[E]`** | **0 E, 0 W, 0 D — every line is `I`** |
| `killall uperf` stops it | verified pre-install; the module's own `uperf_stop` is that path |
| daemon cost | **94 ticks / 10 s ≈ 9% of one core** (was ~107%) |
| device left sane | governors `schedutil`, policy0 at 1804800, `mi_thermald` alive, `tracing_on=0` |

Boot log excerpts worth keeping:

```text
uperf m0(rs-rewrite)[c6f49d4], by grill-glitch (Rust rewrite project)
EventTap: subscribed to 13 topics
Uperf is running
Log level set to 'info'
Input thresholds: swipeThd=0.03 gestureThdX=0.03 gestureThdY=0.03
Atrace disabled
Rust: cpu governor idle (set UPERF_CPU_GOVERNOR=1 to take over frequency control)
Rust: sched scene=idle top=Some("com.android.launcher3") procs=220/220 threads=3042 aff=0 (0 ineffective) prio=2 err=0
```

The `Input thresholds` line is M6c's wiring working with the real `sdm865` config
(`swipeThd=0.03`, i.e. the value 62 of 63 configs ask for and the vendored ctor
hardcodes differently), and `Atrace disabled` is M6d's config gate.

## 4. Attribution

`NOTICE` keeps upstream's text in full (Apache-2.0 requires it) and appends a
rewrite section: the vendored **dfps** (Apache-2.0, commit `f84866c1…`, the one local
change to `input_listener.{h,cpp}`), the **28 Rust crates** statically linked into the
Android artifact with their licence expressions (all MIT/Apache-2.0/Unlicense, none
copyleft), and the five build-only proc-macro crates. It also records what this build
**no longer contains** — jpcre2, nlohmann/json and pcre2 — with the measurement that
justifies dropping them (28 unique ERE patterns, no PCRE-only construct).

`module.prop` is now the rewrite's identity, and its `updateJson` was **removed**:
it pointed at the Game Turbo manifest, which would have offered an "update" that
replaces the rewrite with the closed-source binary.

## 5. Also fixed while shipping

The two compiler warnings in the Android build were real code smells: a thread's CPU
accounting read `/proc/<tid>/stat` for every thread every scan (now lazy, via
`comm_of`), and `pub fn on_event(&mut self, ev: &crate::topic_dispatcher::Event)`
exposed a crate-private type publicly. Both cleaned; the Android build and the host
build are warning-free.

## 6. What §1's other criteria still need

* **criterion 2** (63 configs parse + warnings match upstream verbatim): the parse
  side is covered (`all_configs`, `all_sched_configs`); the *warning-line* comparison
  against upstream has not been done for all 63 — only for the configs exercised here.
* **criterion 3** (all 5 presets × 7 scenes produce the same write sequence as
  upstream): upstream's own write sequence is unobtainable on this kernel (its
  governor and writers cannot start), so this can only be compared against the
  config-derived plan, not against a running upstream.
* **criterion 4** (7 golden traces): needs a working upstream for the same scenarios.
  Our hint FSM's transitions are verified (M4/M6b/M6d), but not side by side with the
  images.
* **polaris** (the second device in §1) has never been touched.

## 7. Known cost of the design

The daemon's steady-state cost is ~9% of a core, and ~0 of that is the Rust engine
(`uperf-sched` is idle between scans, `uperf-watch` is now negligible). The remainder
is the vendored platform layer's listeners, which are upstream's own code and the
thing this project deliberately reuses. **[I]** — not separated further.
