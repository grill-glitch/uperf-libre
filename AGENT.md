# AGENT.md — Uperf Rewrite: Contract, Invariants and Acceptance

> This document states only what holds **now**: contract (§5/§7), architecture invariants (§6/§8), acceptance (§10), rules (§13).
> Historical decisions, per-milestone evidence and resolved unknowns are in [`docs/history/`](./docs/history/).
> Read §6 and §13 before changing code; read §4 before changing shell; read §9 before touching the build.

> This file is the repository's **build instructions and acceptance criteria**. Any agent working in this repository must read this file
> in full first, and self-check against the acceptance items in §10 when committing. **It is forbidden to write "should work" as "verified".**

- Repository: `grill-glitch/uperf-libre` (fork, network root `yc9559/uperf`)
- Content baseline: `yinwanxi/Uperf-Game-Turbo` @ `b13d54a` (= Uperf v3 `dev-22.09.04` binary + 63 platform configs + platform integration scripts)
- Upstream sync branch: `master` (== `upstream/master`, used only for syncing, not touched by this project)
- This project's working branch: `game-turbo` (default branch)

---

## 1. Goals and Boundaries

**Goal**: replace the **closed-source binary** `magisk/bin/uperf` in the repository with our own implementation.
The rewritten business logic is in **Rust**; the C++ platform layer already published by the author's sibling open-source project **dfps** is **reused as-is, not rewritten**.

**Non-goals**:
- Do not change the semantics or values of `config/*.json` (the 63 configs are assets; additions allowed, value changes not).
- Do not change the call contract of `magisk/*.sh` (except the minimal adaptation allowed in §7).
- Do not aim for instruction-level equivalence with the closed-source binary; aim for **config semantic equivalence + runtime behavioural equivalence** (proven with the parity tooling in §10).

**0 closed-source blob invariant**: the binary tree shipped by the module **must not contain any closed-source `.so`**. The SfAnalysis
injection library is rewritten by this repository (`uperf-sfanalysis` → `libsfanalysis_rs.so`); vendor's `libsfanalysis.so` has been
removed from the artifacts, and `build.sh check` asserts this. Mechanism and boundaries: §12.1 and `docs/spec/sfanalysis.md`.

**Success criteria (every one needs reproducible evidence)**:

1. `magisk/bin/uperf` is our own build, the module auto-starts on boot on alioth (crDroid A16 / KernelSU Next),
   and `uperf_log.txt` has no `[E`-level errors (install/boot scripts in §10.3).
2. All 63 `config/*.json` parse, and the **warning/ignored lines match upstream word for word** (parity tooling in §10.2).
3. For configs with `modules.sysfs.enable: true`, the write sequence against a fake root image matches the reference (§8.3 + §10.2).
4. The shutdown path is clean: after `killall uperf` there are no leftover processes, the `userspace` takeover has been reverted,
   and `state=stopped` (§10.3).
5. Release artifacts contain 0 closed-source blobs (`build.sh check`).

---

## 2. Device and Runtime Contract (current constraints)

Only facts that **still constrain the code** are listed; the measurement procedure, evidence chain and conclusions at the time for
each are in [`docs/history/device-findings.md`](./docs/history/device-findings.md).

| Constraint | Consequence |
|---|---|
| alioth's `scaling_max_freq` is **driver-level read-only** (0444; root writes also get EACCES, and `[ -w ]` lies because of `CAP_DAC_OVERRIDE`) | Frequency control can only use `scaling_governor=userspace` + `scaling_setspeed`; check the mode bits before writing, not `access(2)`. See §8.4 |
| Under `/sdcard` (FUSE), root `unlink` **succeeds silently but the file is still there** | Delete via `/data/media/0/...` (which also clears the FUSE view) |
| The daemon and its worker are **both named `uperf`** (dfps rewrites its own cmdline), and `pidof` does not match zombies | Stop the service with `killall uperf`; find live processes via `/proc/<pid>/exe`, not by name or pid file |
| Module path `.` = `/data/adb/modules/uperf`; `USER_PATH=/sdcard/Android/yc/uperf`; CLI contract `uperf <cfg> -o <log>` | See §7 |
| At install time `setup.sh` seeds `config/<soc>.json` into `USER_PATH/uperf.json` (old file backed up as `.bak`), then deletes `config/` from the module | When comparing the zip against the module directory after install, a missing `config/` must be allowed (`scripts/install-and-verify-staged.sh`) |
| No `patchelf` on the device; SF injection requires the library to land on a path tagged `system_file` and `UPERF_SF_INJECT=1` to be set explicitly | By default only the library is installed, surfaceflinger is left alone |
| Local SELinux (Enforcing) rejects `execmem`/`execmod`, and KernelSU-Next has no usable sepolicy channel | SF-side hooks can only use **GOT/PLT rewriting**, never inline hooks |
| `modules.sysfs.enable` is `false` in all 63 shipped configs | Real sysfs writes are explicit opt-in; `RealSink` exists but does not run by default (§8.3) |
| **Known kernel defect**: KSU-Next module mounts + this kernel trigger a dcache panic during shutdown (`list_del corruption` → `__dentry_kill`), `sys.boot.reason=kernel_panic,bug`; after a panic f2fs may lose recently written blocks | After every boot, verify module file md5 and run a full NUL scan (`scripts/post-boot-verify.sh`); never assume the boot was clean |
| `/proc/sys/fs/inotify/*` is writable (`max_queued_events`/`max_user_watches`), but `chown/chmod` gets EACCES in the ksu domain | Tuning is best-effort; on failure print one line |

## 3. Architecture and Choices

### 3.1 Layering

```
┌──────────────────────────────────────────────────────────────┐
│ C++ side (reuses dfps, Apache-2.0, process skeleton)         │
│ main.cpp        process supervisor: fork / SIGCHLD tombstone │
│                 SIGUSR1 restart (hot reload) / log(spdlog)   │
│ platform/       ModuleBase(event bus) CoBridge DelayedWorker │
│                 HeavyWorker Inotifier Singleton              │
│ modules/        InputListener CgroupListener OffscreenMonitor│
│                 TopappMonitor  ← event source, reused as-is  │
│ utils/          inotify input_reader sched_ctrl atrace       │
│                 misc_android backtrace time_counter          │
└───────────────▲──────────────────────────┬───────────────────┘
                │ extern "C" bridge        │ event callbacks
                │ cpp/include/uperf_rs.h   │
┌───────────────┴──────────────────────────▼───────────────────┐
│ Rust side (this repo's rewrite, staticlib `libuperf_rs.a`)   │
│ uperf-config  schema/layer/warn + sysfs plan + governor+sched│
│ uperf-core    daemon(registry) + orchestrator + tasks + C ABI│
│ uperf-dfps    dynamic refresh rate (separate crate/subtree)  │
│ uperf-sfanalysis injection lib (cdylib, own profile)         │
│ uperf-cli     host-side parity tool (not shipped)            │
└──────────────────────────────────────────────────────────────┘
```

**Decision (settled, do not relitigate)**: **C++ owns `main()` / process skeleton / logging / event bus; Rust is linked in as a staticlib and carries every rewritten business module.**
Rationale: the fork+SIGCHLD+tombstone+SIGUSR1 supervisor in `main.cpp` is the easiest piece to get wrong, and dfps has proven it works; keeping it in C++ both maximises reuse and keeps the "rewritten part = Rust" boundary sharp.

### 3.2 Rust dependencies (keep minimal)

| crate | Purpose | Note |
|---|---|---|
| `serde` + `serde_json` | config parsing | not allowed to be replaced by a hand-written parser |
| `regex` | `sched` rule regexes | **PCRE2 is not needed**: the 28 unique patterns across the 63 configs are all plain ERE (no lookaround/atomic/`\K`/backreferences/POSIX word-boundary), and `uperf-config/tests/all_sched_configs.rs` rejects newly added PCRE-only syntax. See §8.5 |
| `libc` | syscall/inotify/prctl/affinity | |
| `log` + self-written formatter | logging | output format must match the original, see §7.3 |
| `thiserror` | error types | `unwrap()` forbidden on runtime paths |
| `once_cell` / `parking_lot` | global state and locks | shared state with the C++ side uses `Mutex`; unguarded `static mut` is forbidden |

No async runtime: the original is "threads + polling + inotify"; switching to tokio would introduce timing differences that cannot be reconciled.

---

## 4. Directory Structure (target)

```
/config/                    63 platform configs (frozen, do not change)
/magisk/                    module skeleton; only bin/uperf needs replacing with our build
  customize.sh service.sh post-fs-data.sh uninstall.sh   ← only the entries Magisk/KernelSU calls directly by name
  script/                  ← everything else lives in script/: initsvc.sh, libuperf.sh, libcommon.sh, libsysinfo.sh,
                             libcgroup.sh, libpowercfg.sh, pathinfo.sh, powercfg_{main,once}.sh,
                             miui_migt.sh, platform_special.sh, vtools_powercfg.sh, webui.sh, dfps.sh,
                             setup.sh, uperf_watchdog.sh
                             (`build.sh check` asserts the root holds only the 4 entries and that script/ is complete —
                              historically a drifting copy lived at the root, including the governor restore path)
/cpp/
  dfps/                     ← vendored from yc9559/dfps (Apache-2.0), original file headers kept
    platform/ modules/ utils/ main.cpp CMakeLists.txt
    DFPS_VENDOR.md          vendor record: upstream commit / file list / local change list
  uperf/
    app_main.cpp            uperf-ised main.cpp (PROC_NAME="uperf", log pattern, CLI)
    bridge.cpp              extern "C" bridge: subscription forwarding, worker/sched/misc wrappers
    include/uperf_rs.h      the C header Rust sees (aligned with the cbindgen output)
    CMakeLists.txt
/rust/
  Cargo.toml                workspace (5 crates)
  uperf-config/             schema/layering/warnings + sysfs planning + governor + scheduler (no unsafe)
  uperf-core/               daemon/tasks/orchestrator/FFI (staticlib)
  uperf-dfps/               dynamic refresh rate (separate crate, git subtree)
  uperf-sfanalysis/         SF injection library (cdylib, separate profile)
  uperf-cli/                pure Rust tooling (offline config/parity use, not shipped with the module)
/docs/                      index in docs/README.md
  spec/                     contracts and specs (config-v3, sfanalysis)
  research/                 upstream / third-party investigation
  records/                  per-milestone design, evidence and on-device run logs (m0..m11)
  history/                  project history: milestone summary, device facts, resolved unknowns
  audit/                    unsafe audit reports
  configs/{upstream,ugt}/   38 upstream baseline configs / 63 UGT configs (read-only snapshots)
/build.sh                   make / pack / install / reboot / clean / check
```

Once `cpp/dfps/**` is vendored it **must not be modified**; if a change is needed, copy it into `cpp/uperf/` and record it in `DFPS_VENDOR.md`.

---

## 5. FFI Contract

### 5.1 Event payloads (C++ → Rust)

⚠️ The original bus passes C++ objects directly (`std::string*`, `std::vector<int>*`). **Rust must never dereference STL objects** — that welds the libc++ ABI in place. The bridge layer must convert on the C++ side before crossing the boundary.

| topic | C++ payload | after bridging (C ABI) | Rust-side type |
|---|---|---|---|
| `cgroup.{ta,fg,bg,re}.list` | `PidList*` (`vector<int>`) | `const int32_t* + size_t` | `&[i32]` |
| `cgroup.{ta,fg,bg,re}.update` | `nullptr` | no payload | `()` |
| `input.touch` | `bool*` | `bool` | `bool` |
| `input.btn` | `bool*` | `bool` | `bool` |
| `input.state` | `InputData*` (3×bool) | `#[repr(C)] struct{bool,bool,bool}` | `InputData` |
| `topapp.pkgName` | `std::string*` | `const char*` (NUL-terminated, lifetime = within the call) | `&str` |
| `offscreen.state` | `bool*` | `bool` | `bool` |
| `sfanalysis.hint` | file protocol (see §7.4) | — | Rust side: inotify / read file |

### 5.2 Rust exports (called by C++)

```c
// cpp/include/uperf_rs.h
int  uperf_rs_start(const char *config_path, const char *log_path);  // 0=success
void uperf_rs_stop(void);                                           // on SIGUSR1
void uperf_rs_reload(void);                                         // config hot reload
void uperf_rs_on_event(const char *topic, const void *data, size_t len);
```

### 5.3 Rust imports (bridge to the C++ platform)

```c
// subscribe to an event (converted internally to a C++ callback -> uperf_rs_on_event)
void uperf_bridge_subscribe(const char *topic);
// Worker
uint64_t uperf_bridge_dw_create(const char *name);
void     uperf_bridge_dw_set(uint64_t handle, void (*cb)(void*), void *ud, int64_t ts_us);
uint64_t uperf_bridge_hw_create(const char *name);
void     uperf_bridge_hw_set(uint64_t handle, void (*cb)(void*), void *ud);
// utilities
int  uperf_bridge_sched_set_prio(int tid, int prio, bool reset_on_fork);
int  uperf_bridge_sched_set_affinity(int tid, const uint8_t *cpumask_bytes, size_t n);
int  uperf_bridge_sched_set_class(int tid, int policy, int prio);
int  uperf_bridge_screen_brightness(void);
int  uperf_bridge_os_version(void);
void uperf_bridge_set_thread_name(const char *name);
```

`uperf_rs.h` must match the `cbindgen` output; CI runs `cbindgen --check`.

---

## 6. Module Ownership Table

| uperf module | Owner | Note |
|---|---|---|
| process skeleton / logging / event bus | **C++ (dfps)** | `main.cpp` + `platform/*`, changing only `PROC_NAME`, the log pattern and the CLI |
| InputListener / CgroupListener / OffscreenMonitor / TopappMonitor | **C++ (dfps)** | reused word for word, topics unchanged |
| inotify / input_reader / sched_ctrl / atrace / misc_android / backtrace / time_counter | **C++ (dfps)** | reused word for word |
| module assembly (original `uperf.cpp`) | **Rust** | `rust/uperf-core/src/lib.rs` |
| config management (original JSON/knob system) | **Rust** | `config/`, including layered overrides and warning text |
| `switcher` (hint state machine + preset + perapp) | **Rust** | `switcher/`, state machine in §8.1 |
| `profile` (scene → parameter table dispatch) | **Rust** | `profile/` |
| `sysfs` writer | **Rust** | `uperf-config/src/sysfs.rs` (5 kinds inferred from path shape + `payload()`) + `uperf-core/src/{orchestrator,real_sink}.rs`, see §8.3 |
| `cpu` frequency governor (governor + load sampling) | **Rust** | `governor/`, see §8.4 |
| `sched` context scheduler | **Rust** | `sched/`, see §8.5 |
| `sfanalysis` listener | **Rust** | `sf/`, see §7.4 / §12.1 |
| `anim_watcher` / `log_level_switcher` / `atrace_switcher` | **Rust** | `anim/`, `logmod/` |
| `CpufreqWriter{Msm,Ppm,Epic}{Base,Fixed,Powersave}` + `Performance` | **Rust** | platform differences (`/sys/kernel/msm_performance/...`, `/proc/ppm/policy/...`, `/dev/cluster{}_freq_{min,max}`) are expressed as strategy tables in Rust |
| `libsfanalysis.so` (SF injection library) → `libsfanalysis_rs.so` | **Rust rewrite** | see §12.1 / `docs/spec/sfanalysis.md`; the vendored artifact is no longer shipped with the module (`build.sh check` asserts 0 closed-source blobs) |
| external watchdog / state file (M9) | **shell + Rust** | `magisk/script/uperf_watchdog.sh` (out-of-process supervision + dead-man recovery + restart within budget), `rust/uperf-core/src/status.rs` (`uperf.state`); see §11 M9 and `docs/records/m9-watchdog.md` |
| runtime knobs (no upstream counterpart) | **Rust** | `rust/uperf-core/src/settings.rs`: all `UPERF_*` environment variables are parsed **once** here into `Settings`; downstream modules take parameters only and no longer call `std::env::var` themselves. A new knob changes only this one place plus a parameter, see §13 |
| daemon assembly / task lifecycle (no upstream counterpart) | **Rust** | `rust/uperf-core/src/daemon.rs` (what exists, in what order it starts and stops), `task.rs` (`Task` trait + `Registry`). Stop order is derived from registration order (reverse), with **one exception**: the CPU governor is stopped first, by name: the userspace takeover is the only state that cannot be recovered in-process |

### 6.0 Task lifecycle (`Task` / `Registry` / `Daemon`)

Adding a new data source used to mean editing five places (static, slot accessor, start block, stop block, task file), and the stop list
was hand-written and unchecked by the compiler — miss one and a thread stays alive while the daemon claims `state=stopped`, with the CPU
governor left on `userspace` (the only state in this project that cannot self-recover). Now:

* A task implements `Task` (`name` / `stop` / `is_running`) and registers with the `Registry`; `stop_all` stops in **reverse** order,
  asserting `is_running()` at each step and writing `task '<name>' stopped` to the log (leaks are visible).
* `Daemon` exclusively owns `Settings` / orchestrator / dfps / dispatcher / `Registry`; `uperf_rs_start` only parses Settings,
  writes `uperf.state` and calls `Daemon::start`. A new subsystem = one stage function in `daemon.rs` + one `push`.
* Every stop goes through `shutdown::stop_and_join` (bounded join). `recorder`/`ctl`/`sf-uprobe`/`sf-binder` previously used an
  unbounded `join()`, so one stuck thread could hang the shutdown path (that is, the disarm path).
* The event channel's sender is now replaceable: on a second start the old receiver is dead, and a `OnceLock` would silently drop every
  subsequent event into the dead channel.

### 6.02 Shutdown signals (`signals.rs`)

* Shutdown signals (SIGTERM / SIGINT / SIGUSR1) are **blocked in the worker before the thread is started** (`cpp/uperf/app_main.cpp::BlockTerminationSignals`, in the forked child branch, before `StartPlatform()`); a `sigwait` thread then calls `uperf_rs_stop()` in **normal thread context** and `_exit(0)`. All threads inherit the mask, so a second delivery cannot interrupt a shutdown already in progress (this was exactly the shape of `killall uperf` running the shutdown path twice on the same thread and deadlocking on the non-reentrant log lock).
* **Never call `uperf_rs_stop()` from a signal handler**: it joins threads, takes `parking_lot`/`std` locks, allocates memory, writes the state file and goes through spdlog — none of it async-signal-safe. The signal can land on a thread already holding those locks — the C++ spdlog sink lock in the dispatcher has no `try_lock` variant; the `CpuTask::stop` timeout fallback in the cpu task takes the `writer` lock again on the same thread. Either way the process does not exit and the governor is left on `userspace`.
* The listener thread is started only **after** the daemon has been placed in its slot: otherwise a signal arriving during startup would `_exit` inside the window where "the governor is armed but the daemon is not published".
* `Dispatcher::join_timeout` must call `ffi::clear_dispatch_tx()` first: the FFI slot holds the sender, so dropping only `_keep_alive` means the receiver never sees `Disconnected` and the thread burns the full 2 s budget (measured on device: 2 s between `caught signal` and `task 'cpu' stopped`).

### 6.03 Frame source ladder (`frame_state.rs`)

Three legs: injected hint (M8) > uprobe (⑪) > binder `--latency` (⑤). The ladder, each leg's last reading and the **sole writer** of
the `uperf_frames.state` state file now all live in `frame_state`:

* Each leg only reports via `publish_uprobe()` / `publish_binder()` / `publish_hint()`; readers only ask `active()` (hint if a hint is
  in effect, otherwise uprobe if uprobe is healthy, otherwise binder) or `last_fps()` (for ⑧ recording). The ladder used to live inside
  `sf_binder` (the degraded leg) and had to reach for `sf_uprobe`'s module-level static — a fourth leg would have meant editing the
  degraded leg's main loop.
* A hint carries no fps while it is in effect (a one-byte hint is not a count), so `active()` returns `source=hint, fps=0` at that
  point — the same behaviour the recorder had before, just converged from scattered `set_last_fps(0.0)` calls into one place.
* The state file's fields are produced in one place, `Published::render()`, and tests pin every key the WebUI parses (including
  "a missing value renders as `-`, not as 0").

### 6.04 Control surfaces (three, each in its own place)

| Surface | Implementation | Who uses it | Note |
|---|---|---|---|
| `webui.sh` subcommands | `magisk/script/webui.sh`, emits `key=value` | **the WebUI's only backend** (`webui/ctl.js`) + adb + all e2e | KernelSU's WebUI can only `exec`; nothing replaces this path |
| ctl socket | `rust/uperf-core/src/ctl_socket.rs`, reverse connect + token/version/pid handshake, command set `PING`/`PONG`/`STATUS` | `tools/ctl-listen` (reference consumer) | for same-domain peers (root/ksu); `STATUS` returns the state file content, so the two sides cannot disagree |
| state file | `uperf.state` / `uperf_watchdog.state` / `uperf_frames.state` | watchdog, `webui.sh`, WebUI | read-only view; each file has exactly one writer |

* Reading `key=value` from state files is converged on the shell side into **`libcommon.sh::state_get`** (all 12 occurrences of
  `sed -n 's/^k=//p'` in `webui.sh` now use it). On the Rust side the write points are `status.rs` and
  `frame_state::Published::render()`, so renaming a field touches two writers plus one shell reader.
* `handshake()` on the ctl socket **must reuse the same `BufReader`** on the connection: peers often send the next command (`STATUS`)
  immediately after reading `OK`; if the handshake builds its own reader, those bytes sit in its buffer and are discarded with it.
  This defect only surfaced when the second command was added to this control surface.
* Do not add `stop`/`start` to `webui.sh`: the module's start/stop semantics live in `libuperf.sh` (`uperf_start` / `uperf_stop`,
  including watchdog stop and governor restore); `webui.sh restart` is already the entry point for that.

### 6.1 Runtime knob surface (`Settings`)

`uperf_rs_start` calls `Settings::from_env(config, log)` at the outermost level, derives the paths for `uperf.state` / `orig_governor.txt` /
`sysfs_orig.txt` / `uperf_frames.state` / `uperf.token`, and then **passes values** to every task.

* Reading environment variables inside a module is not allowed — that makes "which knobs does this daemon have" unanswerable, and forces
  tests to mutate the process environment. `std::env::set_var` is a data race on `environ` in a multithreaded process (Rust 2024 marks it
  `unsafe`); this project's `uperf_rs_start` once tripped over this during reload, and now derives purely.
* Tests construct `Settings` and inject it directly (see `cpu_task`'s `scoped_writer`, `sched_task`'s `Settings::default()`); there is no
  longer a "global lock serialising env".
* Still environment-based: the `sfanalysis` injection library's own `UPERF_SFANALYSIS_*` (it is a separate cdylib and does not share this
  crate's Settings).

---

## 7. Compatibility Contract (the do-not-change list)

### 7.1 CLI

```
uperf <config.json> [-o <log_file>]
```
* Process name = `uperf` (`killall uperf` must be able to stop it).
* Initialisation must complete within 2 s of start (the script does `sleep 2` and then migrates cgroups).
* With no arguments, print help containing `Usage: uperf [-o log_file] config_file` and exit.

### 7.2 Compatible switch files (external dependencies, paths must not change)

| Path | Semantics |
|---|---|
| `modules.switcher.switchInode` (default `USER_PATH/cur_powermode.txt`) | inotify-watched; writing a preset name (or `auto`) switches mode |
| `modules.switcher.perapp` (default `USER_PATH/perapp_powermode.txt`) | per-app mode; `*`=default, `-`=screen off, **must exist**, case-sensitive whole-word match |
| `USER_PATH/uperf_log.txt` | log (set by `-o`) |
| `flag/need_recuser` | startup self-recovery marker (created/removed by the scripts; the directory is not committed); this project does not change it |

### 7.3 Log format (**verified: the README example is from the v1 era and must not be used**)

Evidence chain:
* The only pattern string present in the v3 binary is `%H:%M:%S %L %v` (identical to dfps `source/main.cpp:InitLogger()`'s
  `logger->set_pattern("%H:%M:%S %L %v")`); there is **no** bracketed form like `[%H:%M:%S][%L]`.
* The string `CfgMgr` **does not exist** in the v3 binary; what exists are the module names `CpuGovernor` / `SysfsWriter` /
  `ContextScheduler` / `ProfileSwitcher` / `SfAnalysisListener` / `TopappMonitor` / `CgroupListener` /
  `InputListener` / `AtraceSwitcher` / `AnimWatcher`.
* The v1 binary instead has printf-style strings that bake the tag into the message, such as `CfgMgr: Using [%s] by [%s]`.
* The log examples in the README (the same one in this repo and upstream) still use `/sdcard/yc/uperf/` (the v2 path), whereas v3's USER_PATH
  is `/sdcard/Android/yc/uperf`.

Conclusion: **a v3 log line looks like `13:03:33 I <message>`** (spdlog default sink + `%H:%M:%S %L %v`); the tag is carried by the
message content or the logger name. M0 landed on this pattern (`cpp/uperf/app_main.cpp:InitLogger`).
**"Which log line comes from which logger name" is still not statically determined → from M2 on, the on-device original logs are collected line by line and are authoritative.**

Key lines that must be preserved (the anchors for parity assertions; all original strings were confirmed in the on-device binary):
```
uperf v3(22.09.04)[<hash>], by Matt Yang (yccy@outlook.com)   (daemon banner, format string "{} {}[{}], by {}")
Uperf is running
Config file updated, restart uperf to load new config file
Failed to start uperf(pid=...)
uperf(pid=...) terminated unexpectedly, try to get tombstone
>>> Start of tombstone ... <<< / >>> End of tombstone ... <<<
Cannot find the tombstone
Usage: uperf [-o log_file] config_file
```
> ⚠️ The config-layer log text (`Using [..] by [..]`, `Ignored root/platform/knobs/..`, etc.) has **no identically named strings** in v3,
> which means the v3 config layer was rewritten; the exact wording must be captured on device and must not be copied from the v2 README.

### 7.4 `sfanalysis.hint` file protocol

The SF-side injection library writes a single-byte state out to a file (original implementation: a `lseek+read/write` single-byte state
machine, state codes 0..3). The Rust side must:
1. decide whether to enable it from `modules.sfanalysis.enable` and the module flag;
2. watch the hint file (inotify);
3. map the state to "render start / render lag / render end", and decide render end according to `renderIdleSlackTime`;
4. end the Hint within 66 ms of the render-end signal (the behavioural baseline described in the README).
**The state code to semantics mapping has been statically derived** (6 values idle/switch/trigger/gesture/touch/junk ↔ 0..5, ≥6 = unknown):
see [`docs/records/m1-static-reverse.md`](./docs/records/m1-static-reverse.md) §1.3; implementation in §8.1.

---

## 8. Rust-side Implementation Notes

### 8.1 hint state machine (the mermaid diagram in `config/README.md` is the specification)

```
[*] -> idle
idle    -> touch    : touch / key press
idle    -> switch   : screen on
touch   -> trigger  : finger lifted / scroll starts
touch   -> gesture  : full-screen gesture detected
touch   -> switch   : window animation detected
touch   -> junk     : frame drop detected (sfanalysis)
gesture -> switch / junk
junk    -> touch    : timeout / end
trigger|gesture|switch|touch -> touch : timeout or render end
touch   -> idle     : timeout or render end
```
Each hint has a maximum duration (`modules.switcher.hintDuration.{idle,touch,trigger,gesture,switch,junk}`;
`idle` of 0.0 means no limit by default); the `junk` duration is taken from the **same-named key in the preset/scene** (the `junk` scene).

### 8.2 Config layering (**corrected against the 38 real configs: it is not `base_hint` nesting**)

The measured v3 structure (validated against all of `docs/configs/upstream/*.json`, see `docs/records/m2-evidence.md`):

```jsonc
{
  "meta":     { "name": "...", "author": "..." },
  "modules":  { "switcher": {...}, "atrace": {...}, "sfanalysis": {...},
                "sysfs": {...}, "sched": {...}, "cpu": {...}, "anim": {...},
                "input": {...}, "log": {...} },
  "initials": { "<mod>.<param>": <value>, ... },     // flat dotted keys
  "presets":  { "balance": { "*": {...}, "idle": {...}, "touch": {...},
                             "trigger": {...}, "gesture": {...}, "switch": {...},
                             "junk": {...} }, ... }  // under each preset: scene -> {dotted key: value}
}
```

**Key correction**: the level below `presets.<preset>` **is the scene name directly** (`*` / `idle` / `touch` /
`trigger` / `gesture` / `switch` / `junk`), and `*` is that preset's wildcard default. **None of the 38 configs uses
`base_hint` or `presets.<p>.initials` nesting** (earlier versions of AGENT.md got this wrong, inferring from documentation
rather than data). Layering precedence:

1. `presets[<mode>][<scene>][<dotted>]`
2. `presets[<mode>]["*"][<dotted>]`
3. `initials[<dotted>]`

`modules.*` is a static section, read once at instantiation. **The original warning semantics must be reproduced** (item-by-item
reconciliation in §10.2).

### 8.3 sysfs writer (**rewritten against the measured `modules.sysfs.knob`**)

**Paths do not come from code, they come from config**: `modules.sysfs.knob` is a `{knob name: absolute path}` table.
The upstream binary contains **no** `devfreq`/`llcc`/`ufshc` strings (0 hits in `strings`), so both
`uperf-cli plan` and the device side look paths up in this table (see `docs/records/m4-evidence.md` §1).

The writer kind is **inferred from the path shape** (the schema has no `type` field; the type column in the README is a v1/v2 leftover):

| Path shape | WriterKind | Value format |
|---|---|---|
| `/dev/cpuset/*/cpus` | `CpusetCpus` | cpu mask (`0-3,4-5`) |
| `*/cgroup.procs`, `*/tasks` | `CgroupProcs` | pid list |
| contains `/cpufreq/` and ends in `_freq` | `Cpufreq` | kHz verbatim |
| `*/online` | `PerCpu` | 0/1 |
| everything else (devfreq `*_freq`, `msm_performance/*`, `/proc/ppm/*`) | `String` | verbatim |

> The old table (a guess at 6 kinds: string/percluster/percpu/uxaffinity, etc.) is **void**:
> measurement shows no `{0}` placeholder expansion and no trace of uxaffinity.

Optimisation requirement (the original's core selling point): **diff against the previous action on a switch and skip identical values** (same node, same value → written once).

**Execution side as it stands**: three sinks behind one `Sink` trait.

| sink | When used | Behaviour |
|---|---|---|
| `CollectingSink` | tests / no landing site | collects only |
| `UnderRootSink` | when `UPERF_FAKE_ROOT` is set (offline validation, e2e) | writes to the fake root, with a ledger |
| `RealSink` | **`modules.sysfs.enable: true`** and no fake root | writes real nodes: **fd cache** (opening sysfs costs more than writing), **dedup** (skip if the value equals what this sink last wrote), **record before write**, **read back after write** (values clamped/ignored by the driver count as `ineffective`, not as success) |

All 63 shipped configs are `enable: false`, so **the default behaviour is unchanged**: no real writes. `apply_pending` decides
`fake_root` > `enable` > log `would-write` only, where `enable` is read from config by `Orchestrator::sysfs_enabled()` and the sink is
held by `Daemon` (a single instance — the fd cache and dedup table are only meaningful if reused across plans), shared by three producers
(dispatcher / preset watcher / frame source hint callback).

`WriterKind` semantics are carried by `SysfsWrite::payload()`: `CgroupProcs` emits one PID per line, everything else a single value —
because `serialize_value` renders arrays as `a,b,c`, which is not a form `cgroup.procs` accepts. On-device verification is
`scripts/realsink-device.sh` (the knob table points at regular files under `/data/local/tmp`, so the code path is real and the landing
site is safe).

### 8.4 CPU frequency governor (the six steps in `config/README.md`, implemented one by one)

1. Sampling: sample at `baseSampleTime` when loaded and `baseSlackTime` when idle (sources `/proc/stat` + `/sys/.../scaling_cur_freq`).
2. Demand: `demand = load + (1 - load) * (margin + burst)`; when a cluster's load delta > `predictThd`, use the predicted load and ignore `latencyTime`.
3. Frequency point: amortise the ramp-up delay over `latencyTime`; frequency points below `sweetFreq` get no extra delay.
4. Power limit: PL1=`slowLimitPower` / PL2=`fastLimitPower`, pool capacity `fastLimitCapacity`, recovery scaling `fastLimitRecoverScale`; ignore both limits when `burst != 0`.
5. Guided scheduling: `guideCap` adjusts cluster capacity; `limitEfficiency` limits the energy efficiency of frequency points on low-performance clusters.
6. Write: through the cpufreq write channel of §8.3. Target for a whole period: ~0.5 ms.

Energy model (`modules.cpu.powerModel[]`, in cluster order, fields: `efficiency / nr / typicalPower / typicalFreq / sweetFreq / plainFreq / freeFreq`);
**the typical frequency point is not the frequency ceiling**; above `typicalFreq` the curve is extrapolated.

#### 8.4.1 Energy model solved (exact closed form) [V]

Back-derived from the 25 sets of `opp <freq> pwr <x> cost <y>` that the upstream binary prints on startup
(`docs/records/m5-cpu-governor.md` §1):

```
x = freq_GHz / typicalFreq;   Rp = plainFreq/typicalFreq;   Rs = sweetFreq/typicalFreq
ratio(x) =  x<Rp: Rs·Rp·x   |   x<Rs: Rs·x²   |   x≥Rs: x³
power = typicalPower · ratio(x)                  [W/core]
cost  = power / ((efficiency/100) · freq_GHz)    [W / relative GHz]
```

All 25/25 upstream values reproduce to < 0.0015 (the golden test in `uperf-config/src/cpu.rs`).
`freeFreq` does not appear in the curve fit (the README calls it the "lowest power frequency point", i.e. capacity lower-bound semantics).

#### 8.4.2 Frequency control channel (measured on device) [V]

`scaling_max_freq` is **driver-level read-only** on this kernel (mode 444; root gets EACCES too, no AVC record),
`scaling_driver = qcom-cpufreq-hw`. All 8 upstream `CpufreqWriter*` variants require the min-freq node, and none of them opens
→ **the original uperf v3 cannot control frequency at all on alioth; it exits immediately with `No CpufreqWriter supported for this platform`**.

The usable channel is `scaling_governor=userspace` + `scaling_setspeed` (measured: it follows step by step).
Cost: this is a **complete takeover** (the kernel no longer scales on its own, so a target must be published every period), and if the
process disappears without disarming, the policy stays permanently at the last written frequency point. Therefore `cpp/uperf/app_main.cpp`
has been fixed: the worker calls `uperf_rs_stop()` on `TERM_SIG` (disarm + restore the governor), and the daemon's `SIGTERM/SIGINT`
branch now signals the worker first and then exits (the original called `exit()` directly, leaving an orphan worker).

#### 8.4.3 Implementation and approximations

* Power budget allocation: when limited, give the **loaded** clusters the highest step whose "marginal cost (W/relative GHz) is below a common
  ceiling", and binary-search that ceiling so the total active power lands within `limit` — equal marginal cost is the KKT condition for
  "maximum total capacity under a fixed power limit"; that is my reading [I];
* Power attribution: `cluster_power_at_khz × cluster load` (only busy cores draw power). An unconditional idle floor used to be added, and
  stacked with `limitEfficiency` it pinned idle clusters at high steps, eating most of PL1 and pushing fully loaded cluster0 down to
  403 kHz [V];
* Ramp delay: at most one step up/down per period, jumping straight to the target when `predict` fires — this reproduces the README's
  "discrete sampling makes the measured delay always greater than latencyTime"; **no claim of tick-for-tick agreement with upstream** [I];
* OPP candidate set: upstream only printed a subset of the device table, and no rule fits it (not cost dedup / convex hull / threshold);
  suspected truncated capture [U]. This implementation uses the **full** device OPP table (a superset of upstream's selectable set).

On-device acceptance data (`config/sdm888.json` / balance / idle, PL1=1.0W):
idle c0≈0.88GHz, c1=1574400, c2=1747200 (`limitEfficiency` semantics), pool=15.00;
pinning 4 spinners to cpu0-3 with `taskset 0f` → c0 target **1612800 kHz** (0.216W×4=0.864W ≤ 1.0W;
the next step 1708800 is 1.028W > 1.0W, exactly the highest feasible step under PL1), c1/c2 idle and unaffected; after removing the load it
falls back to 883200 and the pool refills.

### 8.5 Context scheduler (`modules.sched`)

* `cpumask`: name → list of CPU ids.
* `affinity[class][scene]`: `bg/fg/idle/touch/boost` → cpumask name (empty string = do not set).
* `prio[class][scene]`: `0`=skip, `1~98`=SCHED_FIFO, `100~139`=SCHED_NORMAL, `-1/-2/-3`=NORMAL/BATCH/IDLE.
* `rules[]`: `name / regex / pinned / rules[{k,ac,pc}]`, array order = match precedence; `/HOME_PACKAGE/`→launcher package name, `/MAIN_THREAD/`→main thread name (substituted at runtime).
* The scene comes from `initials.sched.scene` (`idle|touch|boost`) plus the foreground/background decision (driven by `topapp.pkgName` / `cgroup.*` events).
* Regexes: **use the Rust `regex` crate; PCRE2 is not needed** (the original §3.2 claim that "PCRE2 is mandatory" was an unverified
  assumption, refuted by the data). Measured across the 63 configs: 2528 pattern occurrences, **28 unique patterns**, all plain ERE: no
  lookaround/lookbehind/atomic/branch-reset/conditional/recursion/`\K`/inline flag/backreference. See `docs/records/m6-sched-evidence.md` §2 and
  `uperf-config/tests/all_sched_configs.rs::no_shipped_pattern_needs_pcre2` (adding PCRE-only syntax fails to compile).
  The full compile self-check is in `all_sched_configs.rs`: 101 sched modules / 1014 process rules / 4048 patterns.
* `pinned` semantics (README line 248): **"always applies rules as a process that is visible at the top level"**, not "first match wins".
* `affinity` values may be **a comma-separated list of multiple cpumask group names** (`sdm8g3.json` uses `"c1,c2"`, 8 places in the tree); the union is taken.
* Shipped configs contain **real defects that upstream tolerates** (`sdm8g2/8g3` never define `affinity.fuck`; `sdm7g1` lacks even
  `prio.fuck`), so a dangling class = no-op for that dimension, recorded in `Anomaly`; the acceptance gate asserts the anomaly set is
  **exactly** the three whitelisted items.

---

## 9. Build

### 9.1 Toolchain

* NDK: same as dfps, `aarch64-linux-android23-*` (r24 verified; r30 works locally but requires re-running §10.1).
* Rust: `aarch64-linux-android` target + `cargo-ndk`.
* `build.sh` tasks: `make | pack | install | reboot | clean | check` (following dfps's build.sh style).

### 9.2 Flow

```bash
# 1) Rust staticlib
cargo ndk -t arm64-v8a -o /dev/null build --release -p uperf-core   # produces libuperf_rs.a
# 2) C++ + link (CMake, following dfps's compile/link options)
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=$NDK/.../aarch64-linux-android23-clang \
      -DCMAKE_CXX_COMPILER=.../aarch64-linux-android23-clang++ -H. -Bbuild/arm64 -G "Unix Makefiles"
cmake --build build/arm64 --target uperf -j
# 3) artifact lands at magisk/bin/uperf, then pack/install
sh build.sh Release make pack install
```

Link options follow dfps: `-ffixed-x18 -Wl,--hash-style=both -fPIE -Wl,-exclude-libs,ALL -Wl,--gc-sections`,
with `-static -flto -O3 -Wl,--icf=all,--lto-O3,--strip-all` added for Release; C++ uses `c++_static` + `-fno-rtti -fno-threadsafe-statics`.
The C side must not use `-fno-exceptions` (the bridge must be able to throw to the upper layer).

### 9.3 Artifact assertions (`build.sh check`, must be scripted)

* `file magisk/bin/uperf` → aarch64 PIE
* `readelf -d` → NEEDED only `libm.so / libdl.so / libc.so`
* stripped (no `.symtab`); RELR/RELRO = full; contains `BIND_NOW`
* size < 3 MB (the original is 1.46 MB; going over means dependency bloat)
* cold start to printing "uperf is running" < 2 s (on device)

---

## 10. Testing and Acceptance

### 10.1 Unit tests (`cargo test --release`, run on host)

288 at present (`uperf-core` 141, `uperf-config` 79, `uperf-dfps` 29, `uperf-cli`/integration 39). They cover what is
**decidable on the host**:

* Config: all 63 `config/*.json` parse; the empty values in `template.json` are valid input; layered overrides
  (`initials` → `presets.*` → `presets.<mode>.<scene>`); the 38 upstream baseline configs are in `uperf-cli`'s integration tests.
* Rule engine: every `sched.rules[].regex` and `k` compiles; `/HOME_PACKAGE/` and `/MAIN_THREAD/` substitution is semantically correct;
  newly added PCRE-only syntax makes `all_sched_configs` **fail to compile** (§8.5).
* Governor maths: `demand` / `predictThd` / power budget / `burst` as pure functions + table-driven; the energy model is <0.0015 against upstream's printed values.
* Mechanism layer: hint state machine (§8.1), frame source ladder (`frame_state`), `SysfsWrite::payload()` per kind,
  `RealSink` (landing / dedup / ledger / read-back), the ctl protocol and `STATUS` framing, `procfs`, `Settings` parsing,
  the task registry and the signal path.

### 10.2 parity tooling (offline, no device needed)

> Keep the two config sets apart: the **upstream v3 release package**, 38 files in `docs/configs/upstream/` (the parser regression baseline);
> **this repository (UGT baseline)**, 63 files in `config/` — that is the shipping target.

```bash
uperf-cli parse  <config.json>                  # structural summary (meta/modules/initials/presets)
uperf-cli warn   <config.json>                  # warning lines (word for word with upstream CfgMgr logs)
uperf-cli plan   <config.json> <mode> <scene>   # layering result + the origin of every key
```

Criterion: across all 63 configs × 5 presets × 7 scenes, the `warn`/`plan` diff against the reference output is 0, or every diff has a
written justification. `plan` additionally marks the origin (`preset[scene]` / `preset[*]` / `init`); the originally planned `expand`
subcommand has been folded into `plan`.

### 10.3 Device verification scripts (the actual entry point for on-device acceptance)

Runnable on the host: `scripts/test_watchdog_host.sh` (watchdog, 109 assertions, including `sh -n` on every script).

Requires alioth:

| Script | What it verifies |
|---|---|
| `scripts/cpufreq-write-probe.sh` | writability of kernel knobs (mode bits vs actual write) |
| `scripts/p1-signal-device.sh`, `p1-signal-burst-device.sh` | shutdown path: 8 rounds of `killall uperf` + 20 back-to-back SIGTERMs, asserting exit, disarm, `state=stopped` |
| `scripts/p2-task-registry-{1task,4tasks}.sh` | task registry and stop order (governor first, the rest in reverse) |
| `scripts/takeover-device.sh` | `userspace` takeover and revert on real sysfs |
| `scripts/realsink-device.sh` | `RealSink` (knob table pointing at regular files under `/data/local/tmp`) |
| `scripts/install-and-verify-staged.sh`, `post-boot-verify.sh`, `post-boot-restart-verify.sh` | install, post-boot file integrity, two hot restarts |
| `tools/uprobe-probe/e2e-*.sh`, `tools/binder-probe/e2e-*.sh`, `tools/ctl-listen/e2e-ctl.sh` | both frame source legs, control surface `STATUS` |

Discipline: sysfs goes through `UPERF_FAKE_ROOT` by default (`RealSink` is the exception — by definition it writes real paths, so point
the knob at `/data/local/tmp` in tests); scheduling must also set `UPERF_SCHED_DRY_RUN=1`; after **every** reboot verify module file
md5 and run a full NUL scan (the kernel defect in §2). `media/*.png` are upstream behavioural baseline screenshots, for comparison only,
not a gate.

### 10.4 Definition of done (hard)

* "Implemented" must come with: `cargo test` output, a `uperf-cli plan` diff, or an on-device log/trace excerpt.
* Forbidden: writing UNKNOWN as VERIFIED; substituting "should/expected" for measurement; declaring config compatibility without running parity.
* Anything unfinished goes into §12, never silently skipped.

---

## 11. Feature List (①..⑪ are the circled numbers in the code)

The queue M11 originally laid out; the numbers have become vocabulary in code comments (`⑪` = uprobe frame counting, etc.), so they stay
here; **per-milestone evidence, on-device conclusions and "what was not done at the time" are in [`docs/history/milestone-log.md`](./docs/history/milestone-log.md)**.

| # | Feature | Default | Code |
|---|---|---|---|
| ① | Watchdog steady-state cost caching (liveness via pid+start_ticks cache, full `/proc` scan only when stale) | on | `magisk/script/uperf_watchdog.sh` |
| ② | Write ledger + restore assertion (changed sysfs knobs record their original value first; rolled back by script on stop/kill) | on (records only when there are writes) | `src/sysfs_ledger.rs`, `libuperf.sh::uperf_restore_sysfs` |
| ③ | Automatic re-injection after SF restart | `UPERF_SF_INJECT=1` | `uperf_watchdog.sh` |
| ④ | Foreground helper file source (`foreground.txt` as the top-app source, C++ monitor as fallback) | on (falls back when the file is absent) | `src/foreground.rs` |
| ⑤ | Direct binder SF `--latency` frame source (degraded leg) | `UPERF_SF_BINDER=1` | `src/sf_binder.rs` |
| ⑥ | Thermal-feedback PL1 throttling (the thermal in §8.4) | with the governor (off by default) | `uperf-config/src/thermal.rs` |
| ⑦ | socket control surface: reverse-connect handshake + `PING`/`PONG`/`STATUS` | `UPERF_CTL_SOCKET` | `src/ctl_socket.rs` |
| ⑧ | Recording channel (sessions keyed by pkg+epoch, ≥3 min, DEFLATE) | `UPERF_RECORD=1` | `src/recorder.rs` |
| ⑨ | Config standard-tier merge | **abandoned** (would touch the config format aligned word for word with upstream) | — |
| ⑩ | Honest 32-bit coverage statement + install guard (32-bit builds explicitly out of scope) | guard always on | `libsysinfo.sh::require_aarch64` |
| ⑪ | uprobe per-thread frame counting (AppOpt-style main leg, perf PMU, no BPF loaded) | `UPERF_SF_UPROBE=1` | `src/sf_uprobe.rs`, `src/frame_state.rs` |

## 12. Open Problems

Only the few that **still** hold and affect judgement. Resolved ones (and how they were broken at the time) are in
[`docs/history/unknowns-resolved.md`](./docs/history/unknowns-resolved.md).

### 12.1 SfAnalysis hook point (rewritten in M8, 0 closed-source blobs)

Conclusion: on alioth both `execmem` and `execmod` are rejected (SELinux + no usable sepolicy channel), so the hook is installed by
**GOT/PLT rewriting** (only needs `mprotect` on a data page), which is also what vendor's static xHook was doing all along.
The injection library `libsfanalysis_rs.so` is built by this repository with its own profile (`panic = "unwind"`, see §9);
by default it only installs the library and does not touch SF; only `UPERF_SF_INJECT=1` injects. Spec and reverse-engineering evidence in
[`docs/spec/sfanalysis.md`](./docs/spec/sfanalysis.md),
[`docs/records/m8-sfanalysis-reverse.md`](./docs/records/m8-sfanalysis-reverse.md); how it was investigated at the time is in
[`docs/history/unknowns-resolved.md`](./docs/history/unknowns-resolved.md) and the original text of §12.1.

### 12.2 Other unknowns

| Item | Status | Impact / approach |
|---|---|---|
| The OPP list upstream prints is a subset of the device table (cluster0 printed 9/17), and no rule fits it (not cost dedup / not convex hull / not a power threshold) | **UNKNOWN** (suspected truncated capture) | This implementation uses the **full** device OPP table (a superset of upstream's selectable set), so the reachable frequency range is only wider |
| Upstream's power budget allocation rule and latency smoothing details | **UNKNOWN** | This implementation is "one OPP per sample, jump only when `predict` fires", not asymptotic approach |
| Whether `modules.input.enable=false` actually stops input listening | **UNKNOWN** | All 63 configs are `true`, and the vendored listener is started by the platform layer before config parsing; setting false only prints one extra log line |
| The semantics of `auto` in `cur_powermode.txt` | **[I]** read as "hand it to the perapp rules" (the binary only has traces of `Internal perapp switcher …`) | Code implements the `[I]` reading; never compared against the original on device |
| `comm` is truncated by the kernel to 15 characters | **known** (not a defect) | `/MAIN_THREAD/` is substituted with the observed `comm`; pattern and thread name come from the same source |
| Both frame source legs were only verified in manual e2e (`tools/uprobe-probe/e2e-shade.sh`, `tools/binder-probe/e2e-frames.sh`) | **known** | Both are off by default; an empty `frame.source=` after boot is normal |

### 12.3 Licence and attribution

* This repository's content, like upstream's, is **Apache-2.0**; the vendored dfps code keeps its "Copyright (C) 2021-2022 Matt Yang" header.
* `NOTICE` needs additions: dfps provenance and commit, the Rust dependency list.
* The original binary kept in the repository is a parity reference only; release artifacts must be replaced with our build.

## 13. Coding Rules

* Rust 2021; `#![deny(unsafe_op_in_unsafe_fn)]`; `unsafe` is only allowed in `ffi.rs` and the syscall wrappers, and each occurrence needs a `// SAFETY:` note.
* No `unwrap/expect/panic` on runtime paths (`panic = "abort"` will not save a performance controller that has just been aborted).
* All cross-thread shared state uses `Mutex`/`Arc`; `static mut` is forbidden.
* Environment variables are read once, only in `settings.rs`; a `std::env::var` appearing in a module counts as a regression.
* C++ side changes are kept minimal and must be recorded in `cpp/dfps/DFPS_VENDOR.md`.
* Commit messages in English, conventional (`feat(governor): ...`), aggregated by category; do not commit build artifacts (`/build` is already in `.gitignore`, but you must still make sure the replacement of `magisk/bin/uperf` is intentional).

---

## 14. Quick Commands

```bash
# sync upstream (touches master only, not game-turbo's business changes)
git fetch upstream && git checkout master && git merge --ff-only upstream/master

# build / pack / install
sh build.sh Release make pack install

# offline parity
cargo run -p uperf-cli -- plan config/sdm855.json balance touch | head -40
cargo run -p uperf-cli -- warn config/sdm855.json

# on device
adb -s <serial> shell su -c 'magisk --install-module /data/local/tmp/uperf-magisk.zip'
adb -s <serial> shell 'cat /sdcard/Android/yc/uperf/uperf_log.txt'
```
