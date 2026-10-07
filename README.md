# uperf-libre

[中文版本 / Chinese (Simplified)](./README.zh-CN.md)

A libre re-implementation of [Uperf Game Turbo](https://github.com/yc9559/uperf)
(closed-source binary, lineage: [Project WIPE](https://github.com/yc9559/cpufreq-interactive-opt) →
[Project WIPE v2](https://github.com/yc9559/wipe-v2) →
[Perfd-opt](https://github.com/yc9559/perfd-opt) →
[QTI-mem-opt](https://github.com/yc9559/qti-mem-opt) →
[Uperf v3](https://github.com/yc9559/uperf) →
[Uperf Game Turbo](https://github.com/yinwanxi/Uperf-Game-Turbo)) with the
closed `magisk/bin/uperf` binary replaced by a Rust core that vendors the
[dfps](https://github.com/yc9559/dfps) C++ platform layer (Apache-2.0) for its
event bus, workers, and inotify glue.

`uperf-libre` ships under the same call contract (`bin/uperf
<USER_PATH>/uperf.json -o <USER_PATH>/uperf_log.txt`, `USER_PATH=/sdcard/Android/yc/uperf`)
and accepts the upstream `config/*.json` schema v3 unchanged, so the 63
platform configs at [`config/`](./config) drop in without modification.

---

## Why

Uperf v3 (`dev-22.09.04`) is a userspace CPU-governor and context-scheduler
that implements most of what kernel-side frequency scaling does and a bit
more (touch/swipe/SfAnalysis hint state machine, dynamic-stune-style cluster
binding, devfreq and LLCC boost). The original distribution ships a single
stripped aarch64 PIE binary, which means the policy and the energy model are
opaque, the source never gets re-audited after release, and any change has to
go through the original author.

`uperf-libre` is the same surface area, re-implemented:

| Layer            | Upstream                              | uperf-libre                                                                                  |
| ---------------- | ------------------------------------- | -------------------------------------------------------------------------------------------- |
| Binary `bin/uperf` | closed-source C++, NDK r24, stripped | Rust staticlib (`uperf-core`) + dfps C++ platform layer (vendored, Apache-2.0)              |
| License          | All-rights-reserved                   | Apache-2.0                                                                                   |
| Source           | Not published                         | Full source in this repository                                                               |
| Schema           | v3 (`config/*.json`)                  | v3, **byte-compatible** — all 63 platform configs are accepted                               |
| Logs             | dfps-style `H:M:S L message`          | Same line format                                                                             |
| SoC coverage      | 63 configs (`sdm855`, `kirin980`, …)   | Same 63 configs (`config/*.json`); new configs can be added without touching existing values  |
| SfAnalysis       | Vendored proprietary `libsfanalysis.so` | Same vendor .so (not re-implemented; see AGENT.md §12.1)                                  |

What stays identical:

- Call contract: `bin/uperf <config> -o <log>`.
- `USER_PATH=/sdcard/Android/yc/uperf` and the rest of the
  `magisk/script/libuperf.sh` script contracts.
- Process name (`uperf`), the `killall uperf` teardown path, and the
  `cur_powermode.txt` / `perapp_powermode.txt` switcher files.

What changes:

- The Rust core is auditable and rebuildable from source.
- The energy model, the OPP → frequency policy, the PL1/PL2 pool arithmetic,
  and the latency-smoothing behaviour are unit-tested
  (`cargo test --release`); the closed binary's exact rounding and tick-by-tick
  choices are not reproducible, so `uperf-libre` matches the *config semantics*
  and *observable behaviour*, not bit-for-bit replay.
- The default `AGENT.md §2` baseline configures conservative PL1=2W. On alioth
  this parks the little cluster lower than stock `schedutil`; tune via the
  shipped per-platform config or write your own.

---

## Features

A userspace CPU-governor + context-scheduler with a hint-driven state machine.
Below is what the daemon does on every tick; the per-feature details are in
[`config/README.md`](./config/README.md) and the device-evidence files at
[`docs/`](./docs).

### CPU & memory control

- **CPU-frequency scaling via `userspace` governor + `scaling_setspeed`**. On
  alioth (`qcom-cpufreq-hw`, `scaling_max_freq` locked at the driver level)
  this is the only path that works; every policy is taken over per cluster.
  See [`docs/m5-cpu-governor.md`](./docs/m5-cpu-governor.md).
- **Energy-model-driven OPP selection**. The per-cluster power / cost curves
  fit the upstream printout to <0.0015 across 25 OPPs on three clusters; the
  marginal-cost budget then allocates the OPP table across clusters under a
  shared PL1/PL2 pool.
- **Devfreq and LLCC boost** — `min_freq` / `max_freq` writes to DDR-bandwidth,
  L3-latency, CPU-LLCC-bandwidth, and the UFS devfreq governor.
- **cgroup, cpuset, and devfreq knob writes** with dedup — repeated writes
  with identical values are skipped to keep the daemon's own wakeup overhead
  low.
- **Cluster affinity / dynamic-stune-style binding** — UI threads of the
  foreground app are moved to big cores; idle and background work is
  restricted from them.

### Hint state machine

A short list of the supported scenes; the full enumeration is in
[`config/README.md`](./config/README.md).

- `None` — idle baseline.
- `Tap` / `Swipe` / `Touch` / `Pressed` — derived from `/dev/input/*` events
  with end-velocity hint for swipe duration.
- `HeavyLoad` — promoted from a touch hint when the system-load metric
  `Σ efficiency(i) × load_pct(i) × freq_mhz(i)` exceeds `heavyLoad` for ≥
  `requestBurstSlack` ms. Released as soon as the metric falls back below
  `idleLoad`; this filters games (sustained high load) from short spikes
  (app launch, photo open).
- `SfLag` / `SfBoost` — reported by the SfAnalysis module injected into
  `surfaceflinger`; rate-limited through a token-bucket so a long-running
  GPU stall does not pin the cluster.
- `AmSwitch` — front-app changes, detected via `ActivityManager` activity;
  used to bind the new app's UI threads early (≈100 ms faster load migration).
- `Standby` — screen-off hint detected through wake-lock updates, not via
  the framework broadcast.
- `WakeUp` — fingerprint / wake-up unlock sequence; promoted to a
  maximum-performance hint for the duration of the unlock animation.
- `SsAnim` — system animation playing (e.g. transition).
- `RenderEnd` / `RenderRestart` — based on surfaceflinger frame-submit
  activity polled every sample. Lets a hint end 200–300 ms after the last
  frame (66 ms with SfAnalysis), reducing waste when the user's input ends
  before the rendering finishes.

### Configuration & UX

- **`config/*.json` schema v3**, byte-compatible with upstream.
- **`magisk/script/libuperf.sh` script contracts** preserved.
- **Inotify-driven hot reload** of `cur_powermode.txt` and
  `perapp_powermode.txt`; per-app presets configurable via `Scene` or
  `sh /data/powercfg.sh <mode>`.
- **WebUI** (KernelSU) shipped at `webui/` and built into the Magisk zip.

### What's intentionally out of scope

- **SfAnalysis** is still the upstream-proprietary `libsfanalysis.so` (see
  `AGENT.md §12.1`). The injected version's surface is not re-implemented in
  `uperf-libre`; if you want a fully-free build, you can drop the
  `libsfanalysis.so` step entirely (the daemon still runs and the CPU governor
  works, you just lose the 66 ms SfLag hint).
- **APK install acceleration** is upstream-supported through a separate
  event; not ported.

---

## Build

```sh
export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make check
```

Artifacts land under `build/aarch64-linux-android23/runnable/uperf`. The
`make check` target runs `cargo test --release` for the Rust core **before**
the CMake build, because a stale `libuperf_core.a` after an FFI signature
change segfaults in `memcpy` (see `AGENT.md §10` and the upstream notes in
[`docs/`](./docs)).

## Install

The Magisk module packaging is unchanged from upstream; `build.sh pack` produces
a KernelSU-compatible zip that drops into `/data/adb/modules_update/uperf-libre`
on next boot. The module id, paths, and the `libuperf.sh` script contracts
are all preserved.

Manual install: unpack the zip anywhere on the device, `chmod 755` the scripts
listed in `setup_uperf.sh`, then run `setup_uperf.sh` and `run_uperf.sh`.

## Verify

After installing, confirm the daemon is up:

```sh
cat /sdcard/Android/yc/uperf/uperf_log.txt | tail
echo powersave > /sdcard/Android/yc/uperf/cur_powermode   # hot reload
```

Graceful stop (the only safe path; SIGKILL leaves the userspace governor
armed at the last `scaling_setspeed` value):

```sh
killall uperf
```

If the device is ever left in `userspace` after a crash, restore with:

```sh
for d in /sys/devices/system/cpu/cpufreq/policy*; do
  echo powersave > $d/scaling_governor
done
```

The stop script records what it replaced at arm time under
`<USER_PATH>/orig_governor.txt`, so a `killall uperf` that succeeds will
restore `schedutil` for you.

## Hardware coverage

63 platform configs at [`config/`](./config). Each one is a drop-in for the
upstream binary: `setup.sh build.sh sdm888.json` is exactly the invocation the
original `yinwanxi/Uperf-Game-Turbo` Magisk module uses.

For a device that is not in the list, the binary still runs but skips the
SoC-specific knobs (`modules.sysfs.knob` resolves to `None` per knob); see
`AGENT.md §11` for adding a new SoC config.

## Architecture

See [`AGENT.md`](./AGENT.md) for the full plan and acceptance criteria, and
[`docs/`](./docs) for milestone-by-milestone device evidence.

```
┌─────────────────────────────────────────────────────────────┐
│ C++ (vendored dfps, Apache-2.0, process skeleton)            │
│  main.cpp        supervisor: fork worker, SIGCHLD tombstone  │
│                  SIGUSR1 graceful reload, spdlog             │
│  platform/       ModuleBase, CoBridge, DelayedWorker,        │
│                  HeavyWorker, Inotifier, Singleton           │
│  modules/        InputListener CgroupListener OffscreenMonitor│
│                  TopappMonitor      ← event sources          │
│  utils/            inotify input_reader sched_ctrl atrace …   │
└─────────────▲──────────────────────────┬────────────────────┘
            │ extern "C" bridge           │ event callbacks
            │ cpp/include/uperf_rs.h      │
┌─────────────▼──────────────────────────▼────────────────────┐
│ Rust (this project, staticlib libuperf_rs.a)                 │
│  app      module wiring (replaces uperf.cpp)                 │
│  config   JSON parse + dot-key overrides + compat warnings   │
│  switcher hint FSM + preset/perapp switching + duration      │
│  profile  preset/scene → per-module parameter dispatch       │
│  sysfs    6 writer kinds + dedup + fd cache                  │
│  governor load sampling + energy model + PL1/PL2 pool + OPP │
│  sched    context-scheduler rule engine (PCRE2-semantics ERE)│
│  sf       sfanalysis.hint consumer + render-end / Hint EOF   │
└─────────────────────────────────────────────────────────────┘
```

The CPU governor takes over each `cpufreq` policy by switching its
`scaling_governor` to `userspace` and publishing the next OPP target through
`scaling_setspeed` every ~20 ms. On devices that have locked
`scaling_max_freq` at the driver level (e.g. `qcom-cpufreq-hw` on alioth —
mode 444, root writes fail with EACCES), this is the only path that works;
see [`docs/m5-cpu-governor.md`](./docs/m5-cpu-governor.md) and the
`freq_target` resolution order in
[`rust/uperf-config/src/freq_target.rs`](./rust/uperf-config/src/freq_target.rs).

## Status

- **Stable**: ground truth for the OPP table, energy model (25 OPPs
  reproducible to <0.0015 against the upstream printout), the
  PL1/PL2 pool arithmetic, the scene → sysfs writer pipeline, and the
  inotify-based hot reload of `cur_powermode.txt` / `perapp_powermode.txt`.
- **Best-effort**: latency smoothing (one OPP per sample unless the
  predict branch fires — upstream describes a continuous shared latency
  budget whose discrete approximation we do not claim to match tick-for-tick),
  and the guideCap / limitEfficiency capacity trim table (not directly
  observable from the closed binary).
- **Verified on**: alioth (crDroid Android 16 / KernelSU Next 3.3.0) and
  polaris (LineageOS 22.2 working; Android 16 / 4.19 kernel — axion config
  with `KERNEL_CLANG_TRIPLE` set).

## Contributing

See [`AGENT.md` §10 acceptance checklist](./AGENT.md) before opening a PR; the
project's parity tool (`rust/uperf-cli`) checks `Config '{}' by '{}'` and
`Knob '{}' not writeable` log lines byte-for-byte against the closed
binary's expected output for every shipped config, and CI fails if the
warning set drifts.

## Acknowledgements

- The closed Uperf v3 binary, configs, and platform scripts at
  [yinwanxi/Uperf-Game-Turbo](https://github.com/yinwanxi/Uperf-Game-Turbo)
  (`b13d54a`).
- The dfps C++ platform layer, vendored under
  [cpp/dfps/](./cpp/dfps) from [yc9559/dfps](https://github.com/yc9559/dfps)
  (Apache-2.0); see `cpp/dfps/DFPS_VENDOR.md` for the vendoring rules.
- The 63 per-platform configs are author-contributed and credited in each
  config's `meta.author` field.

## License

Apache-2.0. See [`LICENSE`](./LICENSE) and [`NOTICE`](./NOTICE).