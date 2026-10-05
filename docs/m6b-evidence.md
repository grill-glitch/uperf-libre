# M6b — preset switching, sfanalysis hint, log level

Status legend: **[V]** verified by tool output · **[I]** inferred · **[U]** unknown.

## 1. The switcher contract, from the configs and scripts [V]

`modules.switcher` names two files, and their formats come from the module's own
scripts/templates rather than from the prose:

```jsonc
"switcher": {
  "switchInode": "/sdcard/Android/yc/uperf/cur_powermode.txt",
  "perapp":      "/sdcard/Android/yc/uperf/perapp_powermode.txt"
}
```

`cur_powermode.txt` — `magisk/script/powercfg_main.sh` writes a single value:

```sh
"powersave" | "balance" | "performance" | "fast" | "auto") echo "$1" >"$USER_PATH/cur_powermode.txt" ;;
"pedestal") echo "performance" >"$USER_PATH/cur_powermode.txt" ;;
"init")     echo "balance"     >"$USER_PATH/cur_powermode.txt" ;;
```

**`auto` is a legal value but is not a preset**: the four preset keys in every
config are `balance/powersave/performance/fast`. Upstream's strings
`Internal perapp switcher {}` / `Internal perapp switcher cannot be enabled`
indicate it hands control to the per-app rules. **[I]** — inferred from those
strings plus the value list; not observed end to end.

`perapp_powermode.txt` — verbatim from `magisk/config/perapp_powermode.txt`:

```text
# 分应用性能模式配置
# Per-app dynamic power mode rule
# '-' means offscreen rule
# '*' means default rule

com.tencent.tmgp.sgame performance
- powersave
* performance
```

i.e. `<package> <preset>`, `#` comments, `-` = offscreen rule, `*` = default rule.
That maps one-to-one onto upstream's strings: `Loading perapp rule`,
`Failed to load perapp rule`, `Default perapp preset not specified`,
`Offscreen perapp preset not specified`,
`Perapp preset '{}' for app '{}' not defined in config`, `Perapp '{}' -> '{}'`.

## 2. Upstream watches these files with inotify [V]

`readelf --dyn-syms` on the release binary shows `inotify_init`, `inotify_add_watch`,
`inotify_rm_watch` and `poll` as imports, plus the `InotifyHandle` class and the
error string `Cannot make inotify handle`. `magisk/script/libuperf.sh` confirms the
intent and raises the kernel limits before starting:

```sh
uperf_start() {
    # raise inotify limit in case file sync existed
    lock_val "1048576" /proc/sys/fs/inotify/max_queued_events
    lock_val "1048576" /proc/sys/fs/inotify/max_user_watches
    lock_val "1024"    /proc/sys/fs/inotify/max_user_instances
```

So inotify it is — including on `/sdcard`. Verified on device: switching worked with
the switcher files on `/sdcard/Android/yc/uperf/` **and** on `/data/local/tmp/`, so the
FUSE mount is not a blocker for *existing* files.

Implementation: `uperf-core/src/inotify.rs` (raw `inotify_init1`/`add_watch`/`poll`,
no crates) + `uperf-core/src/watch_task.rs`. **inotify is used only as a wakeup**:
on any event all three files are re-read from scratch. Attributing an event to a path
looks easy and is not — one `echo x > file` produces both `IN_MODIFY` and
`IN_CLOSE_WRITE`, a rename-into-place arrives on the *directory*, and a file that does
not exist yet cannot be watched at all.

## 3. Four bugs the device round found [V]

1. **A silent thread deadlock.** The startup path called `orch.lock()` twice inside a
   single expression (`orch.lock().top_app()...` and `orch.lock().offscreen()`); the
   first guard is a temporary that lives until the end of the statement and
   `parking_lot::Mutex` is not reentrant, so the watcher thread hung *before arming any
   watch*. Nothing panicked and nothing was logged. Symptom: the startup lines appear
   and then the daemon never reacts to anything again.
2. **`echo x > file` truncates first.** A read can catch the file empty, and reporting
   that logged a bogus `Preset inode -> ''` and could blank the preset on a writer that
   never fills the file in. An empty read is now treated as "no news".
3. **A file's first write is also its creation.** `sfanalysis.hint` may not exist when
   the daemon starts; a watch armed afterwards never sees the write that created it —
   measured on `/sdcard` (the directory-create notification did not arrive). The hint
   byte is now polled every tick (1-byte read + dedup) instead of relying on the watch,
   and all watches are re-armed every tick (`inotify_add_watch` on an already-watched
   path is idempotent).
4. **`modules.sfanalysis.enable` was ignored.** `sdm888.json` ships it as `false`, and
   upstream then logs `SfAnalysisListener disabled by config` and does not listen at
   all. The watcher now drops the hint path entirely when it is disabled.

## 4. `log.level` was hardcoded to debug [V]

Upstream has a `LogLevelSwitcher` class and the level names `trace/debug/info/warn` as
binary literals; **every one of the 63 configs sets `"level": "info"`**. This build
had `logger->set_level(spdlog::level::debug)` in `app_main.cpp`, which no config asks
for. Now `uperf_bridge_set_log_level()` (C++) is called from Rust with
`modules.log.level`, and the C++ default is `info`. Device:

```text
shipped config  (log.level = info)  ->  Log level set to 'info'
synthetic config(log.level = debug) ->  Log level set to 'debug'
```

## 5. `atrace` — contract found, payload unknown [U]

`modules.atrace: { "enable": bool }`, and the binary carries the class
`AtraceSwitcher`, the source path `.../atrace_switcher.cpp`, two candidate files

```text
/sys/kernel/tracing/trace_marker
/sys/kernel/debug/tracing/trace_marker
```

and the failure string `Failed to open tracemark for atrace`. So the module writes to
the ftrace **trace_marker** rather than configuring trace categories itself.

**What it writes is not determinable from the binary** — there is no marker literal
near those strings, so the payload must be built at runtime. It is also disabled in
`sdm888.json`. Implementing only an `open()` with no payload would have no observable
behaviour to verify, so the module is **deferred** rather than shipped hollow. Finishing
it needs the real module installed plus a traced session to see the markers (M7).

## 6. Device verification [V]

`UPERF_FAKE_ROOT` + `UPERF_SCHED_DRY_RUN=1`, `config/sdm888.json` with the switcher paths
left at their shipped `/sdcard/Android/yc/uperf/` values:

```text
Rust: current home is 'com.android.launcher3'
Rust: preset/hint watcher started
Preset inode -> 'balance'
SfAnalysisListener disabled by config
<SfHint byte written>  ...  (only when sfanalysis.enable is true)
Preset inode -> 'performance'
Preset 'balance' -> 'performance'
Rust: preset applied mode=performance writes=16 failed=0     <-- same 16 writes as M4
Preset inode -> 'auto'                                       <-- resolves to the current preset: no change, no log spam
Failed to switch to undefined preset 'turbo'                 <-- value "turbo" is not a preset key
```

With `sfanalysis.enable = true` and the hint file next to the config:

```text
Rust: SfAnalysis hint 'touch' (byte 4) transitioned=true
```

One startup banner, exactly one `Preset inode -> 'balance'` line, and
`kill -TERM` leaves `procs left: 0` with the CPU governors untouched
(`policy0/4/7 = powersave`). The scheduler ran in `[DRY]` mode throughout.

## 7. Two harness hazards worth stating plainly

* **`UPERF_FAKE_ROOT` protects sysfs writes only.** `sched_setaffinity` /
  `sched_setscheduler` are syscalls with no path to redirect, so an early run of this
  test drove the context scheduler against the shipped config on the real device
  (205 processes / 3119 threads examined, 1 policy change applied). Device harnesses
  must set `UPERF_SCHED_DRY_RUN=1` as well.
* **`unlink` under `/sdcard` silently no-ops for root** in the KernelSU context: `rm`
  returns 0 and the file is still there. Deleting requires the underlying path —
  `rm /data/media/0/Android/yc/uperf/cur_powermode.txt` works and clears the FUSE
  view too. That is a plausible explanation for why 5 of the 63 configs use
  `/data/media/0/Android/yc/uperf/...` instead of `/sdcard/...` for the same files.
  (The FUSE dentry cache can also keep `stat` reporting a path that a directory
  listing no longer shows; `ls -A <parent>` is the honest view.)
* **Tear down with SIGTERM, and match the right name.** dfps sets the process name to
  `uperf` for **both** the daemon and its forked worker, so cleanup that matches the
  binary's filename only kills the daemon and orphans every worker — which is exactly
  what happened, leaving twelve live workers (one of them still running the context
  scheduler against the device). Worse, `SIGKILL`-ing the daemon leaves its workers as
  **zombies** reparented to `init`, whose comm is `[uperf]`: `pidof uperf` does not
  match them, so the harness reports "0 processes" while five remain. Count with
  `ps -A -o NAME | grep -c uperf`, tear down with `kill -TERM`, and expect that a
  `-9` teardown is only clearable by a reboot.
* **`su -c 'sh script'` in the foreground is the reliable shape.** Backgrounding the
  *script itself* (`nohup sh script > f 2>&1 &`) loses it: su exits and takes the
  process group with it, so `f` stays empty. Redirecting **inside** the foregrounded
  script is fine. A dropped adb link also truncates the output stream without an error,
  which reads exactly like "the script produced nothing" — check `uptime` and device
  presence before concluding anything about the code.

## 8. Bounded joins on the shutdown path [V]

`uperf_rs_stop()` joins its background tasks so each can release what it owns — in
particular the CPU governor, which must disarm and restore the kernel's original
governors. An unbounded join is a liability: the watcher thread did hang once (the
mutex-taken-twice bug in §3), and a hung task made `uperf_rs_stop()` block forever, so
a worker that the supervisor had signalled never restored the governor. On device,
`kill -TERM` left four workers alive.

`shutdown::stop_and_join` now bounds every join (`STOP_TIMEOUT = 2 s`, ~8x the longest
poll interval), and on timeout the thread is deliberately detached rather than
blocking process exit. The CPU task additionally shares its `UserspaceWriter` with the
handle, so the **stop path can disarm the governor itself** when the task thread never
reached its own `disarm()`. Restoring the governors is the one cleanup this module
cannot afford to skip.

Verified: after `kill -TERM`, the log ends with

```text
Rust: context scheduler stopped
uperf_rs_stop: dispatcher joined
```

and all three policies read `powersave` with no live `uperf` process left, while the
five pre-existing `[uperf]` zombies (from an earlier `SIGKILL` teardown) kept exactly
the same PIDs — i.e. the graceful path added none.

## 9. `killall uperf` used to leave the device pinned in `userspace` [V]

Found by testing **the module's own stop path** instead of one of my own invention:
`magisk/script/libuperf.sh` stops the daemon with `killall uperf`, and that is also what
`uninstall.sh` and any user script would do.

Real run, no `UPERF_FAKE_ROOT`, so the governor genuinely takes over:

```text
armed:                 governors: userspace userspace userspace   live uperf: 2
after killall uperf:   governors: userspace userspace userspace   live uperf: 2
                       disarmed lines: 0
```

Two processes survived and all three policies stayed in `userspace` — a phone pinned at
the last published frequency with no way back except a manual write.

**Cause: a re-entered signal handler deadlocking on a non-reentrant mutex.** `killall`
delivers `SIGTERM` to the daemon *and* the worker at the same time, and the daemon's own
handler then forwards `TERM_SIG` (`SIGUSR1`) to the worker. The worker's `AppSigHandler`
therefore runs **twice, concurrently, on the same thread**; both runs call
`uperf_rs_stop()`, and both log. The shared log line buffer is a plain
`std::sync::Mutex`, which is not reentrant, so the second `lock()` blocked on the first
forever. `AppSigHandler` also previously handled only `TERM_SIG`, so a direct `SIGTERM`
to a worker took the *daemon's* inherited handler and exited **without**
`uperf_rs_stop()` at all.

Two fixes, both needed:

1. `AppSigHandler` now handles `SIGTERM`/`SIGINT` as well as `TERM_SIG` — a direct
   `killall` must run the same cleanup, not the supervisor's bare `exit()`;
2. the handler blocks `TERM_SIG`/`SIGTERM`/`SIGINT` with `sigprocmask` before doing any
   work, so it cannot re-enter;
3. every log helper uses `try_lock` and **drops** the line when the buffer is busy (a
   lost log line is always better than a hung daemon). Verified standalone that
   `try_lock` returns `WouldBlock` immediately in exactly that shape.

After the fix, same test:

```text
after killall uperf:   governors: powersave powersave powersave   live uperf: 0
                       Rust: cpu governor disarmed (original governor restored)
                       uperf_rs_stop: dispatcher joined
```

Signalling the daemon alone was always fine (`t+2s live=0`, governors restored) — the bug
only showed up on the path the module actually uses.

## 10. Still open

* the **producer** of `sfanalysis.hint`: the vendor `libsfanalysis.so` contains no
  path string at all (only `/proc/<pid>/comm`, `/proc/<pid>/stat`, `/proc/self/maps`,
  `/system/bin/surfaceflinger`), so it must receive or build the path some other way.
  The consumer-side path `<config dir>/sfanalysis.hint` is **[I]**. **[U]**
* `auto` semantics **[I]** (see §1).
* `atrace` payload **[U]** (see §5).
* solved in §11 — kept here as the original note of the gap.

## 11. `modules.input.*` wired (the one vendored deviation) [V]

`modules.input` documents five parameters; the README marks two of them unused:

| key | README | consequence |
|---|---|---|
| `swipeThd` | 单次触摸轨迹百分比长度超过该阈值，判定为滑动操作 | **wired** |
| `gestureThdX` | 全面屏手势起始 X 轴百分比位置 | **wired** |
| `gestureThdY` | 全面屏手势起始 Y 轴百分比位置 | **wired** |
| `gestureDelayTime` | **暂不使用** | not wired — correct as-is |
| `holdEnterTime` | **暂不使用** | not wired — correct as-is |

So the real gap was three values, and one of them mattered: **62 of the 63 configs ask for
`swipeThd` = 0.03 while the vendored constructor hardcodes 0.01** — a 3x difference in
swipe detection. Only `sdm888.json`, the config used for development here, uses 0.01,
which is why it never showed up on this device. `uperf-config`'
`the_shipped_swipe_threshold_distribution_is_what_justifies_the_wiring` test pins that
distribution so the claim cannot rot.

dfps hardcodes them in the constructor with no setter, so this needed **the first and only
change to `cpp/dfps/**`**: a 3-line `SetThresholds()` (see `DFPS_VENDOR.md` for the diff
and why a setter beats reimplementing ~300 lines of evdev hotplug). The alternative —
rewriting `InputListener` under `cpp/uperf/` — was rejected as the larger risk.

Plumbing: `app_main.cpp` registers the constructed listener with the bridge, Rust calls
`uperf_bridge_set_input_thresholds()` after parsing the config. Device:

```text
sdm888.json        -> Input thresholds: swipeThd=0.01 gestureThdX=0.03 gestureThdY=0.03
the 62/63 variant  -> Input thresholds: swipeThd=0.03 gestureThdX=0.03 gestureThdY=0.03
```

`modules.input.enable` is **not** honoured: all 63 configs set it true, and the vendored
listener is started by the platform layer before the config is parsed, so a config that
disabled it would be logged but not obeyed. **[U]** — worth closing if a config ever
disables it.
