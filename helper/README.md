# Foreground helper — top app without `dumpsys` (④, work in progress)

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

Not yet verified: that calling those methods actually *returns* the top package here
(the probe enumerated the API surface but was not extended to call it), i.e. whether the
`system_server`-side permission checks accept a root `app_process`.

## Blocker (honest status: this is where ④ stands)

Any dex containing **more than one class** is reliably killed in this environment:

| variant | content | result |
|---|---|---|
| `V1` | single class, writes a file, sleeps 3 s | **rc=0**, prints start and end |
| `V2` | single class + one unused named inner class, sleeps 3 s | **rc=137 (SIGKILL)** |
| `V3` | + `Proxy.newProxyInstance(ITaskStackListener)`, registers, sleeps | **rc=137**, but the registration itself printed its progress first |
| `ForegroundHelper` | the real helper | abort (134) in one form, SIGKILL (137) in another, empty log |

`setsid` (own session/process group) does **not** help. Nothing about it appears in
`logcat -b all` or `-b crash`; the abort we did capture was in `app_process`'s own
`AndroidRuntime::startReg` → `FindClass` → `ASSERT_NO_PENDING_EXCEPTION`, i.e. before any
of our code runs. Not yet identified: *who* kills it (kernel OOM from a pathological dex
verification is the leading hypothesis, unchecked — `dmesg` was not captured in the same
window) and why the class count matters at all.

Next steps, in order of information per minute:

1. Run `V2` with `dmesg | tail -30` and `logcat -d | grep -iE "oom|lowmemory"` in the same
   window — settles whether it is the kernel killing it.
2. Build V2's dex **without** `--release`/`--lib` and with `d8` from a different
   build-tools version — a dex/desugar difference is the other plausible shape.
3. Only once a two-class dex survives: extend the helper to call `getFocusedRootTaskInfo()`
   and confirm it reports the real package across `am start` switches. Then the daemon side
   (read `<USER_PATH>/foreground.txt` per tick, use it as the `topapp.pkgName` source with
   the C++ monitor as fallback) is a small, testable Rust change.

The Java source and the probe are kept here rather than deleted so none of the above has
to be rediscovered. `build/` is a local artifact directory and is not committed.
