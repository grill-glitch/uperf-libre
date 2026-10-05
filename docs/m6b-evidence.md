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

## 8. Still open

* the **producer** of `sfanalysis.hint`: the vendor `libsfanalysis.so` contains no
  path string at all (only `/proc/<pid>/comm`, `/proc/<pid>/stat`, `/proc/self/maps`,
  `/system/bin/surfaceflinger`), so it must receive or build the path some other way.
  The consumer-side path `<config dir>/sfanalysis.hint` is **[I]**. **[U]**
* `auto` semantics **[I]** (see §1).
* `atrace` payload **[U]** (see §5).
* `modules.input.*` (`swipeThd`, `gestureThdX/Y`, `gestureDelayTime`, `holdEnterTime`)
  are still the vendored dfps hardcoded defaults (`0.01/0.03/0.03/2.0/1.0` in
  `input_listener.cpp`) and are **not** read from the config yet. The values used by
  `sdm888.json` happen to equal those defaults, which is why the mismatch is invisible
  on this device but is real for other configs. **[V]**
