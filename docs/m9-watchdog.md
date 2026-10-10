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
| `scripts/test_watchdog_host.sh` | host harness, 8 cases / 48 assertions |

Status: **[V] host-verified** (cargo + the harness below); **device verification
pending** — see §5.

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

`<USER_PATH>/uperf_watchdog.state` — written by the watchdog: `state`
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

**[V] Host, `sh scripts/test_watchdog_host.sh`** — 48 assertions, 0 failures,
against the real script with a fake cpufreq tree, a fake daemon pair built from a
copied shell binary, a real `/proc` (the `exe` uniqueness is what makes that
possible) and a stubbed `uperf_start`; `uperf_restore_governors` is the real one.
Cases: healthy / dead+armed→budgeted restarts then restore / restart succeeds /
orphaned worker retired / single-instance lock / two supervisors collapsed /
`webui.sh status` reporting the new keys / script syntax + module wiring.

The duplicate-supervisor case came out of the harness itself: a case that leaked
its restarted pair made the next one see `sup_n=2`, which the watchdog correctly
treated as unhealthy and collapsed. That accident is now a case of its own (and
each case kills leftovers first, so one case cannot silently change another's
premise).

**[U] Device items, still open:**

* that `/proc/<pid>/exe` really survives dfps' cmdline rewrite on the device
  (the mechanism is standard, but it is the whole identity argument);
* that the temp+rename onto `/sdcard` sticks, or that the direct-write fallback
  is what gets used (both are silent by design);
* that a real `uperf` daemon SIGTERM in the watchdog's teardown path disarms and
  leaves no zombie (`docs/m6b-evidence.md` §9 covered `killall uperf`; the
  watchdog's teardown is the same signal pattern, but this is a new caller);
* that KernelSU/Magisk keep the watchdog alive across the `service.sh` exit
  (`setsid` is used when present) and re-reap it properly;
* the WebUI restart path (`webui.sh restart` → `uperf_stop` + `uperf_start`):
  the lock is released by `uperf_watchdog_stop`, and a new instance starts; the
  poll-then-drop-lock fallback was not exercised against a slow watchdog;
* cost: one `for` over `/proc` plus a `readlink` per pid every 15 s, plus one
  small state write per transition — not measured on device yet.

## 6. Knobs

| variable | default | purpose |
|---|---|---|
| `UPERF_WATCHDOG=0` | — | disable the watchdog entirely (`uperf_start` then starts nothing) |
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
