# M9 — external watchdog + status file

Scope: make the one failure this module cannot recover from in-process
recoverable from outside it. Deliverables:

| | |
|---|---|
| `magisk/script/uperf_watchdog.sh` | the supervisor: liveness from `/proc`, dead-man restore, budgeted restart |
| `rust/uperf-core/src/status.rs` | `uperf.state` — the machine-readable daemon status |
| `magisk/script/libuperf.sh` | `uperf_watchdog_start` / `uperf_watchdog_stop`, wired into `uperf_start` / `uperf_stop` |
| `magisk/uninstall.sh` | stops the watchdog by pid before the module files disappear |
| `magisk/script/webui.sh` | `status` also reports `daemon.state`/`daemon.armed`/`watchdog.*` |
| `scripts/test_watchdog_host.sh` | host harness, 9 cases / 55 assertions |
| `scripts/m9-device-verify.sh` | device harness (on the phone, via `su -c`), 33 assertions |
| `scripts/m9-device-boot-verify.sh` | post-install/post-boot harness, 23 assertions |

Status: **[V] host-verified** (cargo + the host harness: 10 cases / 60 assertions),
**[V] device-verified** (alioth, KernelSU-Next, Enforcing: `m9-device-verify.sh`,
33 assertions) and **[V] install+boot verified** on the same device
(`ksud module install` + a restart: `m9-device-boot-verify.sh`, 23 assertions —
the watchdog is started by KernelSU's `service.sh` path, survives, a WebUI restart
hands the lock over, and the status file survives that restart).

## 1. The problem

Frequency control is a takeover: `scaling_governor=userspace` +
`scaling_setspeed` is the only writable path on alioth (`qcom-cpufreq-hw` locks
`scaling_max_freq`), and once a policy is in `userspace` the kernel stops scaling
it — the last published frequency sticks until the daemon hands the policy back.

Every stop path the daemon can run does that: SIGTERM/SIGINT in
`cpp/uperf/app_main.cpp` (`uperf_rs_stop()`), the task-level fallback in
`CpuTask::stop`, and the script-side net `uperf_restore_governors`
(`libuperf.sh`) on start/stop/uninstall. Two exits bypass all of them:

1. **SIGKILL.** Nothing in-process survives it.
2. **A Rust panic.** `rust/Cargo.toml` builds the release profile with
   `panic = "abort"` (deliberate: §13 forbids panicking on the run path, and an
   abort is cheaper than an unwind), so no destructor runs and `disarm()` never
   executes.

Measured consequence before this milestone: a killed daemon leaves every policy
pinned at the last published frequency until the module is restarted or the
device reboots. `uperf_restore_governors` cannot help on its own because nothing
calls it while the device is still up.

## 2. What the watchdog does

Started by `uperf_start` (last, after the 2 s daemon bring-up), stopped by
`uperf_stop` and by the uninstaller. Every `UPERF_WATCHDOG_INTERVAL` seconds
(default 15):

1. **Liveness from `/proc`, never from names or a pid file.** For every pid it
   resolves `exe` and keeps the ones equal to the module binary. Roles then come
   from the process tree: the *supervisor* is the one whose parent is not another
   process of ours (dfps `setsid`s it, so its parent is init), its children are
   the *workers*. Healthy = exactly one supervisor and at least one worker.
   * Why not the process name: dfps rewrites the cmdline of the supervisor **and**
   the worker to plain `uperf`, `pidof` does not match a zombie, and a `killall`
   teardown that matches the binary's filename kills only the supervisor and
   orphans the worker that owns the governor (`docs/m6b-evidence.md` §9-§11,
   `docs/m7-evidence.md` §1).
   * Why not a recorded pid: a restart can reuse the same pid for the same
   binary. `exe` cannot be reused.
   * The scan filters candidates on `/proc/<pid>/comm` **first**, because a
     `readlink` per pid costs ~13 ms on a 1289-process device (measured: an
     unfiltered pass took 19.9 s, so the loop never finished a sample). The filter
     uses the fixed name dfps gives both processes (`PROC_NAME` = `uperf`, *not* the
     binary's file name — a copy at `/data/local/tmp/m9/fake_uperf` still reports
     `comm=uperf`); `exe` stays the authority, so a process that merely *names*
     itself `uperf` is rejected. `UPERF_WATCHDOG_COMM` overrides the name.
   * `readlink` reports ` (deleted)` while a running daemon holds a binary that a
   module update replaced; that suffix is stripped.
2. **Unhealthy for `UPERF_WATCHDOG_GRACE` consecutive samples** (default 2; the
   config-reload window is ~1.5 s) → take down whatever is left with **SIGTERM**.
   That is exactly what the module's own `killall uperf` does, and it is the path
   that calls `uperf_rs_stop()`. A SIGKILL here would recreate the failure.
3. **If the takeover is still visible**, restore the recorded originals through
   the same `uperf_restore_governors` the stop script uses — it only touches a
   policy while that policy reads `userspace`, and never invents a value. The
   live `scaling_governor` value, not a recorded one, is the authority on whether
   a takeover is in effect.
4. **Restart**, at most `UPERF_WATCHDOG_MAX_RESTARTS` times per boot (default 3),
   via the normal `uperf_start` (config self-heal, cgroup placement, log
   rotation) with `UPERF_WATCHDOG_SUPPRESS=1` so no nested watchdog is spawned.
   `UPERF_WATCHDOG_SUPPRESS` is not the only guard: a second instance also exits
   on the owner lock.
5. **Once the budget is spent**: restore, write `state=gave-up`, log, and stop
   restarting. A device running the platform governor is strictly better than one
   pinned at a frequency chosen before the daemon died.

Two escalation details:

* If SIGTERM did not retire the leftovers, the governors are restored **before**
  the `SIGKILL` — so the kill cannot leave a policy pinned — and that is logged
  as the one place a SIGKILL is defensible.
* A module that is being deleted (`$SCRIPT_PATH/libuperf.sh` gone) makes the
  watchdog exit instead of fighting a binary that no longer exists.

It does **not** touch frequency targets, cgroup placement, or the config, and it
does not replace the in-process paths — it is the third line, after them and after
the script-side restore.

## 3. The two state files

Each has exactly one writer, so nothing interleaves.

`<USER_PATH>/uperf.state` — written by the daemon (`status.rs`), on startup, on
arm/disarm, and on the clean stop path. Never trusted for liveness.

| key | meaning |
|---|---|
| `state` | `running` while the daemon is up, `stopped` on the clean stop path |
| `takeover` | whether this run was started with `UPERF_CPU_GOVERNOR=1` |
| `armed` / `policies` | number and names of the policies currently in `userspace` |
| `pid` / `ppid` / `start_ticks` | who the daemon is, and since when (PID-reuse defence) |
| `boot_id` | boot identity, so a stale file from a previous boot is recognisable |
| `uptime_ms` | monotonic timestamp of the write |
| `config` | the config file this daemon loaded |

A **stale `state=running` with no live process is the documented signature of a
kill** — the state the watchdog reacts to. That is also why `uperf_rs_stop`
writes `state=stopped`: after a clean stop the file says so.

`<USER_PATH>/uperf_watchdog.state` — written by the watchdog **every sample**, even
when nothing changes (`updated_uptime_ms` is the evidence that a supervisor with
nothing to report is still alive; a log line every 15 s would drown the log). Fields:
`state`
(`starting`/`running`/`recovering`/`restarted`/`restart-failed`/`restored`/
`gave-up`/`stopped`), `restarts`, `interval_s`, `max_restarts`, the last
sup/worker/armed snapshot, `exe`, `pid`/`boot_id`/`start_ticks` and
`updated_uptime_ms`. A stop after a give-up keeps the history in `detail`
(`stopped by the module (last state: gave-up)`), because "stopped" alone would
hide why the platform governor is in charge.

Both are written through a same-directory temp file plus `rename`, with a direct
write as the fallback: `/sdcard` is FUSE and a root `unlink` there can silently
no-op on this device (`docs/m6b-evidence.md` §6), so `rename` is not assumed to
stick — and status reporting must not depend on it either.

`uperf_watchdog.log` records every decision (start, each unhealthy sample, the
teardown, the restore, the restart outcome, the give-up, the stop). It is capped
at 128 KB, trimmed to the last 200 lines at startup.

## 4. Failure-mode table

| situation | before M9 | now |
|---|---|---|
| daemon healthy | — | left alone; `state=running` |
| daemon SIGKILLed while armed | pinned until reboot / module restart | SIGTERM leftovers → restore → restart; `state=restarting` → `running` |
| daemon panicked (`panic = "abort"`) | same | same |
| daemon dead, restart keeps failing | — | after the budget: restore, `state=gave-up`, log |
| supervisor killed, worker orphaned (still armed) | orphan keeps the governor armed, nothing owns it | orphan detected as a supervisor without workers → retired → restore |
| two supervisors (e.g. a raced restart) | two engines fighting | `sup_n != 1` is unhealthy → all torn down, one restarted |
| module stopped on purpose (`uperf_stop`) | — | watchdog stopped first, so it cannot fight the stop |
| module uninstalled | — | watchdog killed by pid, then the usual restore |
| module files deleted while running | — | watchdog exits on its own |

## 5. Evidence

**[V] Host, `cargo test --release`** — 211 tests across the workspace, 0
failures. New: `status::tests::*` (5) plus
`cpu_task::tests::userspace_writer_reports_status_on_arm_and_disarm`.

Two things this milestone found on the host, both fixed:

* `UPERF_FAKE_ROOT` is process-wide, and two tests in `cpu_task::tests` were
  pointing it at different fake roots concurrently — the loser's writes landed in
  the other test's tree (`original governor must be restored` failing, left =
  `userspace`). Every test that touches it now takes `FAKE_ROOT_LOCK`.
* The watchdog redirected `USER_PATH`/`FLAG_PATH` after `libuperf.sh` had already
  frozen `GOVERNOR_STATE` and `WATCHDOG_LOCK_DIR` from the device paths, so the
  restore looked for the governor record in `/sdcard`. `GOVERNOR_STATE` is
  re-derived after the seams and the lock dir is now late-bound
  (`uperf_watchdog_lock_dir()`); the host harness is what caught it — a
  redirected tree restored nothing.

**[V] Host, `sh scripts/test_watchdog_host.sh`** — 55 assertions, 0 failures,
against the real script with a fake cpufreq tree, a fake daemon pair built from a
copied shell binary, a real `/proc` (the `exe` uniqueness is what makes that
possible) and a stubbed `uperf_start`; `uperf_restore_governors` is the real one.
Cases: healthy / dead+armed→budgeted restarts then restore / restart succeeds /
orphaned worker retired / single-instance lock / two supervisors collapsed /
`webui.sh status` reporting the new keys / script syntax + module wiring.

**[V] Device, `scripts/m9-device-verify.sh` on alioth** (crDroid Android 16,
kernel `4.19.325-cip131`, KernelSU-Next, SELinux Enforcing): **31 assertions, 0
failures.** The real CPU is never taken over — the takeover is exercised against a
fake sysfs root (`UPERF_FAKE_ROOT`) and the real governors are compared before and
after; the installed module's daemon is only observed (`UPERF_WATCHDOG_DRY_RUN=1`).

* **`/proc/<pid>/exe` survives the cmdline rewrite.** The installed daemon reports
  `cmdline=[uperf]` for both processes while `exe=/data/adb/modules/uperf/bin/uperf`
  — supervisor `ppid=1`, worker `ppid=<supervisor>`. The whole identity argument,
  checked against the real thing instead of a fake.
* The watchdog judged that pair **healthy with its real pids**, read the real
  cpufreq tree (`armed=[]`), produced no false unhealthy sample, and left the
  module alone (same pids before and after).
* `/sdcard`: the status file's temp+rename sticks and reads back; `rm` also worked
  — the FUSE-unlink caveat from M6b did not reproduce on this build.
* **Dead-man path, real daemon:** armed into the fake root (all three policies
  `userspace`), then both processes SIGKILLed → the takeover stayed armed → the
  watchdog restored all three through the real `uperf_restore_governors` and left
  no zombie.
* **Graceful path, real daemon:** only the supervisor SIGKILLed → the orphan is
  classified as a lone supervisor → the watchdog SIGTERMs it → **the daemon's own
  handler disarms it** (`cpu governor disarmed`), with the policies back before the
  watchdog's restore would have run.
* **Cost at the shipped 15 s cadence: 56-67 ticks = 560-670 ms over 60 s =
  0.93-1.12 % of one core** across three runs; one scan ≈ 0.13-0.19 s (1289
  processes).

The raw log is kept at [`docs/m9-device-run-alioth.txt`](./m9-device-run-alioth.txt)
(the harness also writes a `.completed` marker on the device, so a truncated log
cannot pass for a finished run — that failure mode cost two rounds here).

Three defects only the device could show, all fixed:

1. **The scan was too slow to ever finish a pass.** 1289 pids × a `readlink|sed`
   pipeline, with a fork costing ~10-13 ms here, took **19.9 s**; the loop logged
   nothing but start/stop because it never completed a sample. Fixed by filtering on
   `comm` (a shell builtin: 1289 pids in 0.19 s) and parsing the stat fields with
   `read` + parameter expansion (`set -- ${line##*) }`) instead of `sed|awk`. The
   host harness cannot see this class: a desktop has ~200 processes and 30× cheaper
   forks.
2. **The filter's name is not the image's file name.** The first fix derived it from
   `basename "$DAEMON_EXE"`, but dfps sets `PROC_NAME` for both processes, so the
   test binary at `.../fake_uperf` reported `comm=uperf` and the watchdog saw
   **zero** processes. The default is now that fixed name.
3. **`read` returns non-zero at EOF *without a newline*.** The daemon writes the
   governor value with no trailing newline (`fs::write(.., "userspace")`), so
   `IFS= read -r g <.../scaling_governor || continue` skipped **every armed policy**
   and reported `armed=[]` where all three were armed — and the armed check is what
   decides whether a restore is needed. The `|| continue` is gone; the value
   comparison decides. The host harness now writes its armed values without a
   newline for the same reason.

The duplicate-supervisor case came out of the harness itself: a case that leaked
its restarted pair made the next one see `sup_n=2`, which the watchdog correctly
treated as unhealthy and collapsed. That accident is now a case of its own (and
each case kills leftovers first, so one case cannot silently change another's
premise).

**[V] Device, install + boot** (`scripts/m9-device-boot-verify.sh`, **23
assertions, 0 failures**, raw log in
[`docs/m9-boot-run-alioth.txt`](./m9-boot-run-alioth.txt)). The module was installed
with `ksud module install` (staged binary md5 == the local build) and the device
restarted:

* **The watchdog is started by KernelSU's own path and survives it.** 60 s after
  boot: `watchdog started pid=6812 boot=<this boot>` at 11:33:17, then
  `healthy: sup=[6466] workers=[6467] armed=[]`; the lock owner
  `6812:<boot_id>:3583` matches the live process' `start_ticks`, its cmdline is
  `sh /data/adb/modules/uperf/script/uperf_watchdog.sh` (so the `setsid` launch
  works), the state file carries *this* boot's `boot_id`, and it was still
  sampling 14 s before the check (i.e. it is ticking at the 15 s cadence, not a
  stale file).
* `uperf.state` from the installed build reports `state=running`,
  `takeover=off`, `armed=0` — the new `status.rs` shipped and works.
* **The WebUI restart hands the lock over:** `webui.sh restart` → daemon back,
  lock owned by a *new* watchdog, the old one gone, and the new instance reporting
  the daemon healthy. No double supervision.
* **The status file survives a fast restart.** The boot run caught the opposite:
  `uperf.state` said `stopped` beside a running daemon. `uperf_stop` (SIGTERM,
  `sleep 1`) and `uperf_start` overlap — the *previous* worker's `uperf_rs_stop()`
  joins its threads for up to 2 s, so its `state=stopped` write can land after the
  new worker claimed the file, and since the takeover is off by default the new
  daemon never writes again: the file lies for the rest of the session. The stop
  paths now go through `status::write_if_owner()`, which refuses to touch a file
  whose `pid=` is not this process. Verified two ways: five consecutive
  `webui.sh restart`s each ended `state=running` with `pid` equal to the live
  worker (the failing condition, measured in a loop), and the boot harness asserts
  that ownership on every run.

**[U] Device items, still open:**

* the `comm` filter's failure mode: a daemon whose process name is neither the
  default nor `UPERF_WATCHDOG_COMM` looks absent. It is only a *filter*, so the
  consequence is a wrong verdict rather than an action on a stranger's process —
  but it is an assumption about a name dfps sets in `cpp/uperf/app_main.cpp`.
* the WebUI *app* does not display the new status keys yet (the keys themselves are
  host- and device-verified through `webui.sh status`; surfacing them is UI work);
* the poll-then-drop-lock fallback in `uperf_watchdog_stop` was not exercised
  against a watchdog that refuses to die within 5 s;
* a full day of uptime (the log cap's long-run behaviour is inferred from the
  rotation being size-driven — see `docs/m10-log-cap.md`).

## 6. Knobs

| variable | default | purpose |
|---|---|---|
| `UPERF_WATCHDOG=0` | — | disable the watchdog entirely (`uperf_start` then starts nothing) |
| `UPERF_WATCHDOG_COMM` | `uperf` | process name to filter candidates on (the name dfps sets, *not* the file name) |
| `UPERF_WATCHDOG_SUPPRESS=1` | — | internal: do not spawn a nested watchdog (set by the watchdog itself) |
| `UPERF_WATCHDOG_INTERVAL` | 15 | healthy sampling period, seconds |
| `UPERF_WATCHDOG_RETRY_INTERVAL` | 3 | sampling period while unhealthy but inside the grace window |
| `UPERF_WATCHDOG_GRACE` | 2 | consecutive unhealthy samples before acting |
| `UPERF_WATCHDOG_MAX_RESTARTS` | 3 | restart budget **per boot** |
| `UPERF_WATCHDOG_TEARDOWN_TICKS` | 5 | seconds to wait for a SIGTERM to land |
| `UPERF_WATCHDOG_VERIFY_WAIT` | 3 | seconds to wait after a restart before judging it |

Test seams (defaults = the device layout): `UPERF_WATCHDOG_{PROC_ROOT,
CPUFREQ_ROOT,EXE,USER_PATH,FLAG_PATH,LOG,STATE,STUB}` and
`UPERF_CPUFREQ_ROOT`, which `uperf_policy_dirs` also honours — one definition of
"where the policies are", shared by the module and the harness.
