# Foreground helper — top app without `dumpsys` (④)

**Why**: `cpp/dfps/source/modules/topapp_monitor.cpp` learns the top app from
`GetTopAppNameDumpsys()` — a spawned process per lookup — and so gates the lookup on the
top-app cgroup's pid *count* moving by more than `TOP_TASK_NR_DIFF_MIN` (10). A switch
between two apps whose process counts differ by fewer than ten pids is simply invisible.
The gate is a cost workaround, not a property of the problem; AppOpt's answer (and this
one) is a source that is *notified* instead of polled.

## Verified facts (device: alioth, crDroid A16, kernel 4.19.325, Enforcing)

Measured with `FgProbe` — a single-class, reflection-only dex run as
`CLASSPATH=probe.jar app_process /system/bin FgProbe` **as root**:

* `android.app.ActivityTaskManager.getService()` returns
  `android.app.IActivityTaskManager$Stub$Proxy` — reflection reaches the framework from a
  bare `app_process`, so the hidden-API filter (which applies to apps) does not.
* `registerTaskStackListener(ITaskStackListener)` exists with exactly that one parameter —
  an event-driven source is available.
* `getFocusedRootTaskInfo()` exists (one call, exactly the question being asked), as does
  `getTasks(int, boolean, boolean, int)`. The 1-argument `getTasks(int)` does **not** exist
  on this build.

## Resolution — 2026-10-10 (device-verified)

Two things stood in the way; both are now settled.

### 1. The "any multi-class dex is SIGKILLed" blocker does not reproduce

The class-count matrix was re-run on the current boot (`helper/exp/`), capturing rc,
timing and `dmesg` in one window:

| variant | classes | result |
|---|---|---|
| `V1` | 1 | **rc=0** |
| `V2` | 2 | **rc=0** (×10, 0 failures) |
| `V1big` | 1 (10 methods) | **rc=0** |
| `V3` | 3 | **rc=0** |
| `ForegroundHelper` | 2 | ran, registered, served |

`logcat` shows `AndroidRuntime: Calling main entry V2` / `V3` for each run; no OOM, no
`lowmemorykiller`, no SIGKILL, no `dmesg` line at the moment of the run. The earlier
`rc=137` was therefore **device-state dependent** — it cleared with the reboot between then
and now, and is *not* a property of the dex. What state it was, is still **[U]** (a `dmesg`
capture was never taken in the failing window); nothing about this environment kills a
two-class dex today. The `ForegroundHelper`'s "abort (134) / SIGKILL (137), empty log" rows
in the first report are the same story.

### 2. The real defect was field-vs-getter in `packageOf`

With the blocker gone, the helper ran and registered — but `foreground.txt` read `-`
(no package). `FgProbe2` dumped the actual object: `getFocusedRootTaskInfo()` returns
`android.app.ActivityTaskManager$RootTaskInfo`, and `topActivity` / `baseActivity` /
`origActivity` / `realActivity` are **public `ComponentName` fields**, not getters —
`getClass().getMethod("topActivity")` throws `NoSuchMethodException`. The lookup had been
getter-only, so *every* task resolved to null. Fixed by reading the field first and falling
back to a getter (`ForegroundHelper.packageOf` + `pkgOfComponent`).

`FgProbe2` also confirmed the API answers the real question:
`getFocusedRootTaskInfo().topActivity.getPackageName()` = `com.android.settings` while
Settings is top, `com.android.launcher3` at home.

## Verified on device (after the fix)

`CLASSPATH=foreground.jar app_process /system/bin ForegroundHelper <out> 1200`, driven by
`helper/exp/fhtest.sh` (`am start` switches):

| step | helper reported |
|---|---|
| t0 (home) | `com.android.launcher3` |
| `am start -a android.settings.SETTINGS` | `com.android.settings` |
| home again | `com.android.launcher3` |
| `am start -n com.android.documentsui/.files.FilesActivity` | `com.android.documentsui` |

The listener's own transitions appear on stderr (`topapp <pkg> @ …`), and the file is
rewritten `<package> <uptime_ms>` on every poll **and** every callback (atomic tmp+rename).
Prerequisite on this device: unlock first (`input keyevent KEYCODE_WAKEUP` +
`wm dismiss-keyguard`), or `am start` is refused while the keyguard is up.

## Daemon side (done — queue item ④ closed)

`rust/uperf-core/src/foreground.rs` is the consumer. Each dispatcher tick re-reads
`<USER_PATH>/foreground.txt` and, when the timestamp is fresh, feeds the line as a
`topapp.pkgName` event through the *same* path a C++-published one takes; a stale or absent
file yields nothing, so the vendored monitor stays the fallback. Wired in
`topic_dispatch.rs` (`recv_timeout` tick + a shared `handle`) and `lib.rs`
(`foreground::from_env(cfg_dir)`).

* `UPERF_FOREGROUND=0` disables the source (C++ monitor only, as before).
* `UPERF_FOREGROUND_FILE` overrides the path; default `<config dir>/foreground.txt`.
* `UPERF_FOREGROUND_MAX_AGE_MS` (default 10000) — must exceed the helper's poll interval.
* `UPERF_FOREGROUND_TICK_MS` (default 500).

Device e2e (`helper/exp/fg-e2e.sh`, real built daemon under `UPERF_FAKE_ROOT`):
`Rust: foreground helper file source = …/foreground.txt` at startup, then
`Rust: topapp.pkgName = com.android.settings / com.android.launcher3 /
com.android.documentsui` matching the `am start` switches, and the scheduler summary
`top=Some("com.android.documentsui")`.

## Not done yet

Nothing **starts** the helper. Launching it — and restarting it if it dies — belongs in the
module's start path (beside `uperf_watchdog.sh`); until then the file source is inert and
the C++ monitor is the only source in a normal boot. The helper's poll interval must stay
below `UPERF_FOREGROUND_MAX_AGE_MS`.

`helper/build/` (compiled classes + jars) is a local artifact directory and is not
committed; `build.sh` builds the dex by hand (see its header — it is deliberately not wired
into the repo build). The device harness scripts (`exp/*.sh`) and the probes
(`FgProbe.java`, `FgProbe2.java`) **are** committed, so none of this has to be
rediscovered. `exp/` in order: `exp.sh` (class-count matrix), `v2loop.sh` (flakiness),
`fhtest.sh` (helper across switches), `fg-e2e.sh` (daemon reads the file).
