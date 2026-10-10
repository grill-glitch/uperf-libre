#!/system/bin/sh
#
# uperf watchdog — external supervisor and dead-man switch for the daemon (M9).
#
# Why this exists
# ---------------
# The CPU governor drives frequency through `scaling_governor=userspace`
# (`docs/m5-cpu-governor.md` §2 — the only writable path on a `qcom-cpufreq-hw`
# kernel). That is a *takeover*: once armed, the kernel stops scaling the policy
# and only the daemon can hand it back. Every stop path the daemon can run
# restores the originals (SIGTERM/SIGINT in `cpp/uperf/app_main.cpp`, the
# task-level fallback in `rust/uperf-core/src/cpu_task.rs`), but two exits cannot:
#
#   * SIGKILL — nothing in-process survives it;
#   * a Rust panic — `rust/Cargo.toml` builds the release profile with
#     `panic = "abort"`, so no unwinding destructor runs.
#
# In both cases the policies stay pinned at the last published frequency until the
# module is restarted or the device reboots. `uperf_restore_governors`
# (`libuperf.sh`) is the script-side net, but it only runs on start/stop/uninstall.
# This watchdog is the third line of defence, and it is the only one that reacts
# while the device is still running.
#
# What it does, every UPERF_WATCHDOG_INTERVAL seconds (default 15):
#
#   1. Liveness from /proc, not from names or from a pid file. dfps rewrites the
#      cmdline of *both* the supervisor and its worker to plain `uperf`, `pidof`
#      does not match a zombie, and a restart can reuse the same pid — so
#      identity comes from `/proc/<pid>/exe` (the module binary, which survives
#      the rewrite) and roles from the process tree (supervisor = the one whose
#      parent is not another of ours; its children are the workers).
#   2. Unhealthy for UPERF_WATCHDOG_GRACE consecutive samples -> take whatever is
#      left down with SIGTERM (never SIGKILL first: the handler is what disarms).
#   3. If the takeover is still visible afterwards, restore the recorded original
#      governors — the same `uperf_restore_governors` the stop script uses, which
#      never invents a value and touches a policy only while it reads `userspace`.
#   4. Restart the daemon, at most UPERF_WATCHDOG_MAX_RESTARTS times per boot.
#      Once that budget is spent it restores, logs, and stops restarting: a device
#      running the platform governor is strictly better than one pinned at a
#      frequency chosen before the daemon died.
#
# It is started by `uperf_start` and killed by `uperf_stop`, so an explicit stop is
# never fought, and a single instance is enforced with an owner lock
# (pid + boot id + start time — the same identity triple that tells a stale lock
# from a live one across a reboot).
#
# It does NOT touch frequency targets, cgroup placement, or the config: it only
# reacts to "the daemon is gone while the takeover is still armed", and to "the
# daemon is gone at all" while the module is meant to be running.
#
# `UPERF_WATCHDOG_DRY_RUN=1` samples and reports but never acts: no teardown, no
# restore, no restart. Same idea as `UPERF_SCHED_DRY_RUN` on the Rust side — the
# only way to watch the judgement on a device whose daemon must not be touched.
#
# Test seams (all default to the device layout): UPERF_WATCHDOG_{PROC_ROOT,COMM,
# CPUFREQ_ROOT,EXE,USER_PATH,FLAG_PATH,LOG,STATE,INTERVAL,RETRY_INTERVAL,GRACE,
# MAX_RESTARTS,TEARDOWN_TICKS,VERIFY_WAIT,STUB}. `scripts/test_watchdog_host.sh`
# drives the whole policy on a host machine with a fake /proc + fake cpufreq tree.

BASEDIR="$(dirname "$(readlink -f "$0")")"
. "$BASEDIR/pathinfo.sh"
. "$BASEDIR/libcommon.sh"
. "$BASEDIR/libuperf.sh"

FLAG_PATH="${UPERF_WATCHDOG_FLAG_PATH:-$FLAG_PATH}"
USER_PATH="${UPERF_WATCHDOG_USER_PATH:-$USER_PATH}"
PROC_ROOT="${UPERF_WATCHDOG_PROC_ROOT:-/proc}"
CPUFREQ_ROOT="${UPERF_WATCHDOG_CPUFREQ_ROOT:-${UPERF_CPUFREQ_ROOT:-/sys/devices/system/cpu/cpufreq}}"
DAEMON_EXE="${UPERF_WATCHDOG_EXE:-$BIN_PATH/uperf}"
# The process name the kernel reports for our processes. It is NOT the image's file
# name: dfps sets it to a fixed string (`PROC_NAME` in cpp/uperf/app_main.cpp) for
# both the supervisor and the worker, so a copied/renamed binary still calls itself
# `uperf` (measured on alioth: image /data/local/tmp/m9/fake_uperf, comm `uperf`).
#
# The name is what makes the scan affordable: reading `comm` is a shell builtin,
# while a `readlink` per pid costs ~13 ms on this device (measured: a full 1289-pid
# exe scan took 19.9 s, so the watchdog never finished a single sample). `exe` stays
# the authority on identity — comm is only a filter, so a process that merely *names*
# itself `uperf` is still rejected.
DAEMON_COMM="${UPERF_WATCHDOG_COMM:-uperf}"
WD_LOG="${UPERF_WATCHDOG_LOG:-$USER_PATH/uperf_watchdog.log}"
WD_STATE="${UPERF_WATCHDOG_STATE:-$USER_PATH/uperf_watchdog.state}"
WD_LOCK_DIR="$FLAG_PATH/uperf_watchdog.lock"
WD_INTERVAL="${UPERF_WATCHDOG_INTERVAL:-15}"
WD_RETRY_INTERVAL="${UPERF_WATCHDOG_RETRY_INTERVAL:-3}"
WD_GRACE="${UPERF_WATCHDOG_GRACE:-2}"
WD_MAX_RESTARTS="${UPERF_WATCHDOG_MAX_RESTARTS:-3}"
WD_TEARDOWN_TICKS="${UPERF_WATCHDOG_TEARDOWN_TICKS:-5}"
WD_VERIFY_WAIT="${UPERF_WATCHDOG_VERIFY_WAIT:-3}"
# Liveness is answered from the cached pid set while it still checks out; the full
# /proc sweep is paid only when the cache goes bad (a pid gone, a start time moved) or
# every $WD_BACKSTOP samples. The backstop exists for the one thing a cache cannot
# see: a *second* pair that appears while the first is healthy (a start that raced the
# lock). 20 samples = 5 min at the shipped interval; the host harness sets it to 1 to
# test that path on purpose.
WD_BACKSTOP="${UPERF_WATCHDOG_BACKSTOP:-20}"

# ------------------------------------------------ SF injection supervision (M8, opt-in)
#
# The borrow from fas-rs's analyzer lifecycle: a frame source that can *disappear*
# (surfaceflinger is restarted, the device reboots, the mapping is lost) needs something
# that notices and re-attaches — with a counter and a give-up that says why. Without it
# the module runs happily with no frame source and nothing outside can tell.
#
# Off unless `UPERF_SF_INJECT=1`, so the default sample costs one string test: the
# product-side decision is that a real injection into surfaceflinger stays opt-in. The
# injector is NOT shipped — it is a ptrace tool from the test setup — so
# `UPERF_SF_INJECTOR` says where it is, and a missing one disables this loop with one log
# line instead of failing silently every sample.
SF_ENABLE="${UPERF_SF_INJECT:-0}"
SF_TARGET="${UPERF_SF_TARGET:-/system/bin/surfaceflinger}"
SF_LIB="${UPERF_SF_LIB:-$BIN_PATH/libsfanalysis_rs.so}"
SF_INJECTOR="${UPERF_SF_INJECTOR:-/data/local/tmp/injector}"
SF_MAX_INJECTS="${UPERF_SF_MAX_INJECTS:-3}"
SF_RETRY_SAMPLES="${UPERF_SF_RETRY_SAMPLES:-4}"
SF_ID=""
SF_INJECTS=0
SF_FAILS=0
SF_STATE="off"
SF_SINCE=999
SF_DISABLED=0
SF_GAVE_UP_LOGGED=0
SF_INJECTED_ID=""
SF_SEEN=0
WD_CACHE=""
WD_SCANS=0
WD_SINCE_SCAN=0

# `uperf_restore_governors` enumerates policies through `uperf_policy_dirs`, which
# honours this; exporting it keeps the one definition of "where the policies are".
export UPERF_CPUFREQ_ROOT="$CPUFREQ_ROOT"

# `libuperf.sh` computed these from the device paths *before* the seams above could
# redirect them; re-derive so a redirected tree is honoured (a restore that looks
# for the governor record in the wrong directory refuses to touch anything, which
# is the safe failure — but it is a failure).
GOVERNOR_STATE="$USER_PATH/orig_governor.txt"

# Host tests inject a stub that replaces `uperf_start` (which would otherwise
# launch the real daemon and touch /dev/cpuset). Sourced last, so it wins.
if [ -n "${UPERF_WATCHDOG_STUB:-}" ] && [ -f "$UPERF_WATCHDOG_STUB" ]; then
    . "$UPERF_WATCHDOG_STUB"
fi

# ---------------------------------------------------------------- plumbing

wd_log() {
    echo "$(date '+%m-%d %H:%M:%S' 2>/dev/null) $*" >>"$WD_LOG" 2>/dev/null
    return 0
}

# Sleep in 1 s slices. A trap only runs once the foreground command returns, so a
# single long `sleep` would delay shutdown by up to one interval — and then a
# restart right after a stop would find the lock held by a watchdog already on its
# way out, and start nothing.
wd_sleep() {
    local n="$1"
    while [ "$n" -gt 0 ]; do
        sleep 1
        n=$((n - 1))
    done
    return 0
}

wd_now_ms() {
    awk '{printf "%d", $1 * 1000}' /proc/uptime 2>/dev/null
}

# `/proc/<pid>/stat` -> WD_PPID and WD_START_TICKS, in one read and no forks.
#
# The fields are counted *after* the `(comm)` field, which may itself contain spaces
# and parentheses, so they are located from the last `)`. After `(comm) `: 1 state,
# 2 ppid, ..., 20 starttime (field 22 overall). `read` + parameter expansion +
# positional parameters are all shell builtins; the `sed|awk` version this replaces
# cost two forks per call, and a fork is ~10 ms here.
wd_stat_fields() {
    local line
    WD_PPID=""
    WD_START_TICKS=""
    # The *group's* stderr is redirected, not the command's: when a pid vanishes
    # mid-scan the shell prints the failed redirection itself.
    { IFS= read -r line <"$PROC_ROOT/$1/stat"; } 2>/dev/null || return 1
    # shellcheck disable=SC2086 # word splitting is the point
    set -- ${line##*) }
    WD_PPID="$2"
    WD_START_TICKS="${20}"
    return 0
}

# The module binary's image. `exe` survives the cmdline rewrite both of our
# processes go through, which is why identity is taken from here and not from the
# process name. A module update replaces the file under a running process, so the
# ` (deleted)` suffix readlink then reports is stripped.
wd_exe() {
    local exe
    exe="$(readlink "$PROC_ROOT/$1/exe" 2>/dev/null)"
    case "$exe" in
    *" (deleted)") exe="${exe% (deleted)}" ;;
    esac
    echo "$exe"
}

# Classify every process running $DAEMON_EXE.
#   WD_SUP  supervisor: no parent inside the set (dfps' daemon, setsid'ed)
#   WD_WORK its workers (the forked app that owns the governor)
#   WD_ALL  both, for teardown
wd_scan() {
    local p pid comm exe
    WD_ALL=""
    for p in "$PROC_ROOT"/[0-9]*; do
        # `read` from /proc/<pid>/comm first: a builtin, so ~1289 pids cost ~0.2 s
        # instead of ~20 s of `readlink|sed`. Only a name match pays for the
        # `readlink`, and the exe comparison below is what actually decides.
        comm=""
        { IFS= read -r comm <"$p/comm"; } 2>/dev/null
        [ "$comm" = "$DAEMON_COMM" ] || continue
        pid="${p##*/}"
        exe="$(wd_exe "$pid")"
        [ "$exe" = "$DAEMON_EXE" ] || continue
        WD_ALL="$WD_ALL $pid"
    done

    WD_SUP=""
    WD_WORK=""
    for pid in $WD_ALL; do
        wd_stat_fields "$pid" || continue
        case " $WD_ALL " in
        *" $WD_PPID "*) WD_WORK="$WD_WORK $pid" ;;
        *) WD_SUP="$WD_SUP $pid" ;;
        esac
    done
    return 0
}

# Classify "<pid>:<start_ticks>:<ppid>" records with the rule `wd_scan` uses, as pure
# string work: no /proc reads and no forks.
wd_classify() {
    local rec pid inner ppid
    WD_ALL=""
    for rec in $1; do WD_ALL="$WD_ALL ${rec%%:*}"; done
    WD_SUP=""
    WD_WORK=""
    for rec in $1; do
        pid="${rec%%:*}"
        inner="${rec#*:}"
        ppid="${inner#*:}"
        case " $WD_ALL " in
        *" $ppid "*) WD_WORK="$WD_WORK $pid" ;;
        *) WD_SUP="$WD_SUP $pid" ;;
        esac
    done
    return 0
}

# Can the cache be reused as-is? A matching pid *and* start time is the same identity
# rule the owner lock uses, and it is two `read`s of /proc/<pid>/stat — no forks, no
# sweep. `exe` is deliberately NOT re-read here: it is what decides *membership* of the
# set, and membership is decided at every sweep (docs/m9-watchdog.md §6). A pid whose
# start time moved is a different process and fails the check.
wd_cache_fresh() {
    local rec pid st
    [ -n "$WD_CACHE" ] || return 1
    for rec in $WD_CACHE; do
        pid="${rec%%:*}"
        st="${rec#*:}"
        st="${st%%:*}"
        wd_stat_fields "$pid" || return 1
        [ "$WD_START_TICKS" = "$st" ] || return 1
    done
    return 0
}

# Rebuild the cache from a fresh sweep.
wd_cache_from_scan() {
    local pid out=""
    for pid in $WD_ALL; do
        wd_stat_fields "$pid" || continue
        out="$out $pid:$WD_START_TICKS:$WD_PPID"
    done
    WD_CACHE="${out# }"
    return 0
}

# Policies currently reading `userspace` — the live sysfs value. This, not the
# status file and not a recorded value, is the authority on whether a takeover is
# in effect.
# Names of the policies currently reading `userspace`, space separated. `read`
# instead of `cat` (one fork per policy per sample, on a hot loop).
wd_armed_list() {
    local d g out=""
    for d in "$CPUFREQ_ROOT"/policy*; do
        [ -f "$d/scaling_governor" ] || continue
        # No `|| continue` on the read: `read` returns non-zero when the file ends
        # without a newline, and the daemon writes this value with no newline at all
        # (`fs::write(.., "userspace")`). Treating that as a failure reported
        # `armed=[]` on a device where every policy was armed.
        g=""
        IFS= read -r g <"$d/scaling_governor" 2>/dev/null
        [ "$g" = "userspace" ] || continue
        out="$out${d##*/} "
    done
    echo "$out"
}

# ---------------------------------------------------------------- owner lock

wd_lock_owner() { cat "$WD_LOCK_DIR/owner" 2>/dev/null; }

wd_lock_owner_alive() {
    local owner pid boot start cur
    owner="$(wd_lock_owner)"
    [ -n "$owner" ] || return 1
    pid="${owner%%:*}"
    owner="${owner#*:}"
    boot="${owner%%:*}"
    start="${owner#*:}"
    [ "$boot" = "$WD_BOOT_ID" ] || return 1
    [ -n "$start" ] || return 1
    [ "$start" != "0" ] || return 1
    wd_stat_fields "$pid" || return 1
    [ -n "$WD_START_TICKS" ] || return 1
    [ "$WD_START_TICKS" = "$start" ] || return 1
    return 0
}

wd_lock_acquire() {
    mkdir -p "$WD_LOCK_DIR" 2>/dev/null || return 1
    if wd_lock_owner_alive; then
        return 1
    fi
    printf '%s:%s:%s\n' "$$" "$WD_BOOT_ID" "$WD_SELF_START" >"$WD_LOCK_DIR/owner" 2>/dev/null || return 1
    WD_OWNED=1
    return 0
}

wd_lock_release() {
    [ "$WD_OWNED" = "1" ] && rm -rf "$WD_LOCK_DIR" 2>/dev/null
    return 0
}

# ------------------------------------------------- SF injection supervision
#
# The target's identity is `pid:start_ticks`, the same rule the daemon pair uses: a
# restarted surfaceflinger is a different process, and that is exactly the event this
# exists to notice. `wd_sf_is_injected` / `wd_sf_do_inject` are functions, not inline
# calls, so the harness can replace them (its stub file is sourced last, like the one
# that replaces `uperf_start`).

SF_COMM="${SF_TARGET##*/}"

wd_sf_pid() {
    local p pid comm
    for p in "$PROC_ROOT"/[0-9]*; do
        comm=""
        { IFS= read -r comm <"$p/comm"; } 2>/dev/null
        [ "$comm" = "$SF_COMM" ] || continue
        pid="${p##*/}"
        wd_stat_fields "$pid" || continue
        echo "$pid:$WD_START_TICKS"
        return 0
    done
    return 1
}

wd_sf_is_injected() {
    grep -q "$(basename "$SF_LIB")" "$PROC_ROOT/$1/maps" 2>/dev/null
}

wd_sf_do_inject() {
    "$SF_INJECTOR" "$SF_TARGET" "$SF_LIB" >/dev/null 2>&1
}

wd_sf_state() {
    SF_STATE="$1"
}

wd_sf_check() {
    local id pid
    [ "$SF_ENABLE" = "1" ] || return 0
    [ "$SF_DISABLED" = "1" ] && return 0

    id="$(wd_sf_pid)"
    if [ -z "$id" ]; then
        [ "$SF_STATE" != "absent" ] && wd_log "sf: $SF_TARGET is not running"
        # The identity is kept: the target coming back with a new pid is a *restart*
        # (worth saying) and only a target that was never seen is a first sight.
        wd_sf_state "absent"
        return 0
    fi
    pid="${id%%:*}"

    if wd_sf_is_injected "$pid"; then
        if [ "$SF_ID" != "$id" ]; then
            wd_log "sf: pid=$pid already carries $(basename "$SF_LIB") (no injection needed)"
        fi
        SF_ID="$id"
        SF_SINCE=0
        # A mapping *we* put there is a different fact from one that was already there,
        # and `healthy` would hide which one it is.
        if [ "$SF_INJECTED_ID" = "$id" ]; then
            wd_sf_state "injected"
        else
            wd_sf_state "healthy"
        fi
        return 0
    fi

    # Not injected. Three cases that look the same from here and are not: first sight, a
    # restarted surfaceflinger, and a process that lost the mapping. All three end in the
    # same action, but only the restart is worth a log line of its own.
    SF_SINCE=$((SF_SINCE + 1))
    if [ "$SF_INJECTS" -gt 0 ] && [ "$SF_ID" = "$id" ] && [ "$SF_SINCE" -lt "$SF_RETRY_SAMPLES" ]; then
        wd_sf_state "waiting"
        return 0
    fi
    # The budget counts *failures*, not attempts: a device whose surfaceflinger restarts
    # often must still get its frame source back on the fifth restart, while an injector
    # that keeps failing (or a mapping that never appears) has to stop being retried.
    if [ "$SF_FAILS" -ge "$SF_MAX_INJECTS" ]; then
        if [ "$SF_GAVE_UP_LOGGED" != "1" ]; then
            wd_log "sf: gave up after $SF_FAILS failed injection attempt(s) of $SF_INJECTS — no frame source (UPERF_SF_MAX_INJECTS)"
            SF_GAVE_UP_LOGGED=1
        fi
        wd_sf_state "gave-up"
        return 0
    fi
    if [ ! -x "$SF_INJECTOR" ]; then
        SF_DISABLED=1
        wd_sf_state "disabled"
        wd_log "sf: no usable injector at $SF_INJECTOR — SF supervision disabled (the module does not ship one)"
        return 0
    fi

    if [ "$SF_SEEN" = "1" ] && [ "$SF_ID" != "$id" ]; then
        wd_log "sf: $SF_TARGET restarted ($SF_ID -> $id), re-injecting"
    fi
    SF_SEEN=1
    SF_INJECTS=$((SF_INJECTS + 1))
    SF_SINCE=0
    wd_log "sf: inject attempt $SF_INJECTS/$SF_MAX_INJECTS into pid=$pid"
    if wd_sf_do_inject "$pid"; then
        # fas-rs verifies its own writes back (`verify_freq`, every 3 s) rather than
        # trusting the call: an injector that returns 0 while the mapping never appears is
        # a failure, not a success.
        wd_sleep 2
        if wd_sf_is_injected "$pid"; then
            SF_ID="$id"
            SF_INJECTED_ID="$id"
            wd_sf_state "injected"
            wd_log "sf: injection took — maps carry $(basename "$SF_LIB")"
        else
            SF_FAILS=$((SF_FAILS + 1))
            SF_ID="$id"
            wd_sf_state "failed"
            wd_log "sf: injector returned success but the mapping is absent — counting a failure"
        fi
    else
        SF_FAILS=$((SF_FAILS + 1))
        SF_ID="$id"
        wd_sf_state "failed"
        wd_log "sf: injector failed on pid=$pid"
    fi
    return 0
}

# ---------------------------------------------------------------- status file

wd_state_body() {
    echo "version=1"
    echo "state=$1"
    echo "detail=$2"
    echo "pid=$$"
    echo "boot_id=$WD_BOOT_ID"
    echo "start_ticks=$WD_SELF_START"
    echo "restarts=$WD_RESTARTS"
    echo "max_restarts=$WD_MAX_RESTARTS"
    echo "interval_s=$WD_INTERVAL"
    echo "sup=$WD_LAST_SUP"
    echo "workers=$WD_LAST_WORKERS"
    echo "armed=$WD_LAST_ARMED"
    echo "exe=$DAEMON_EXE"
    echo "updated_uptime_ms=$(wd_now_ms)"
    echo "sweeps=$WD_SCANS"
    echo "sf=$SF_ID"
    echo "sf_state=$SF_STATE"
    echo "sf_injects=$SF_INJECTS"
    echo "sf_fails=$SF_FAILS"
    echo "sf_max_injects=$SF_MAX_INJECTS"
    echo "since_sweep=$WD_SINCE_SCAN"
    echo "backstop=$WD_BACKSTOP"
}

# Same-directory temp + rename where it works, direct write where it does not: a
# root `unlink` under /sdcard can silently no-op on this device
# (docs/m6b-evidence.md §6), so `mv` is not assumed to stick.
wd_state_write() {
    WD_LAST_WRITTEN="$1"
    wd_state_body "$1" "$2" >"$WD_STATE.new" 2>/dev/null
    if mv "$WD_STATE.new" "$WD_STATE" 2>/dev/null; then
        return 0
    fi
    wd_state_body "$1" "$2" >"$WD_STATE" 2>/dev/null
    rm -f "$WD_STATE.new" 2>/dev/null
    return 0
}

wd_rotate_log() {
    local size
    [ -f "$WD_LOG" ] || return 0
    size="$(wc -c <"$WD_LOG" 2>/dev/null)"
    [ -n "$size" ] || return 0
    [ "$size" -gt 131072 ] || return 0
    tail -n 200 "$WD_LOG" >"$WD_LOG.new" 2>/dev/null && mv "$WD_LOG.new" "$WD_LOG" 2>/dev/null
    return 0
}

# ---------------------------------------------------------------- recovery

# SIGTERM only, and to the supervisor *and* the workers: that is exactly what the
# module's own `killall uperf` teardown does, and it is the path that calls
# `uperf_rs_stop()` and disarms the governor. A SIGKILL here would recreate the
# failure this script exists to clean up.
wd_teardown() {
    local pid i
    for pid in $WD_SUP $WD_WORK; do
        kill -TERM "$pid" 2>/dev/null
    done
    [ -n "$WD_SUP$WD_WORK" ] && wd_log "SIGTERM -> [$(echo $WD_SUP $WD_WORK)]"
    i=0
    while [ "$i" -lt "$WD_TEARDOWN_TICKS" ]; do
        wd_scan
        [ -n "$WD_ALL" ] || return 0
        wd_sleep 1
        i=$((i + 1))
    done
    return 1
}

# `uperf_restore_governors` reports what it did on stdout; route that into the
# watchdog log so the recovery is reconstructible from one file.
wd_restore_governors() {
    local out
    # Both halves of what a killed daemon may have left behind: the governor takeover
    # (libuperf.sh's recorded originals) and the sysfs knobs (the daemon's write
    # ledger). One log line each, so the recovery is reconstructible from one file.
    out="$(uperf_restore_governors 2>&1)"
    [ -n "$out" ] && wd_log "restore: $out"
    out="$(uperf_restore_sysfs 2>&1)"
    [ -n "$out" ] && wd_log "restore-sysfs: $out"
    return 0
}

wd_recover() {
    # This path kills things: work off a fresh sweep, never off the cache (the cache is
    # what broke, so what it holds is exactly what cannot be trusted).
    wd_scan
    wd_cache_from_scan
    # No "restart N" here: whether this recovery restarts is decided below, once
    # the takeover has been undone.
    wd_log "recovering: sup=[${WD_SUP# }] workers=[${WD_WORK# }] armed=[$WD_LAST_ARMED] restarts=$WD_RESTARTS/$WD_MAX_RESTARTS"
    wd_state_write "recovering" "daemon gone; tearing down and restarting"

    if ! wd_teardown; then
        # SIGTERM was not enough. Undo the takeover *before* the kill so a dying
        # process cannot leave a policy pinned, then kill and say so — this is the
        # only place a SIGKILL is defensible.
        wd_log "SIGTERM did not retire [${WD_ALL# }] — restoring the governors first, then killing"
        wd_restore_governors
        for pid in $WD_ALL; do
            kill -KILL "$pid" 2>/dev/null
        done
        wd_sleep 1
        wd_scan
    fi

    WD_LAST_ARMED="$(wd_armed_list)"
    if [ -n "$WD_LAST_ARMED" ]; then
        wd_log "takeover still armed ([$WD_LAST_ARMED]) after teardown — restoring the recorded governors"
        wd_restore_governors
        wd_state_write "restored" "daemon did not disarm; restored from the recorded governors"
    fi

    if [ "$WD_RESTARTS" -ge "$WD_MAX_RESTARTS" ]; then
        wd_log "restart budget spent ($WD_RESTARTS/$WD_MAX_RESTARTS) — leaving the platform governor in charge"
        wd_restore_governors
        wd_state_write "gave-up" "restart budget spent; platform governor in charge"
        WD_OBSERVE_ONLY=1
        # Sticky: from here on the state file keeps saying `gave-up` however many
        # unhealthy samples follow. A later `unhealthy` write would erase the one
        # fact a reader needs (why the platform governor is in charge), and the
        # stop path would lose it too.
        WD_GAVE_UP=1
        return 1
    fi

    WD_RESTARTS=$((WD_RESTARTS + 1))
    wd_state_write "restarting" "attempt $WD_RESTARTS/$WD_MAX_RESTARTS"
    wd_log "restarting the daemon (attempt $WD_RESTARTS/$WD_MAX_RESTARTS)"
    # `uperf_start` brings the daemon up the normal way (config self-heal, cgroup
    # placement, log rotation). Suppress the nested watchdog: this shell already
    # owns the lock.
    export UPERF_WATCHDOG_SUPPRESS=1
    uperf_start
    unset UPERF_WATCHDOG_SUPPRESS

    wd_sleep "$WD_VERIFY_WAIT"
    wd_scan
    set -- $WD_SUP
    local sup_n=$#
    set -- $WD_WORK
    local work_n=$#
    if [ "$sup_n" -eq 1 ] && [ "$work_n" -ge 1 ]; then
        wd_log "restart ok: sup=[${WD_SUP# }] workers=[${WD_WORK# }]"
        wd_state_write "running" "restarted by the watchdog (attempt $WD_RESTARTS)"
        return 0
    fi
    wd_log "restart did not bring the daemon up (sup_n=$sup_n work_n=$work_n)"
    wd_state_write "restart-failed" "attempt $WD_RESTARTS did not bring the daemon up"
    return 1
}

wd_shutdown() {
    wd_log "watchdog stopping: $1"
    # Carry the last state into the detail: after a `gave-up` run, "stopped" on its
    # own would hide the fact that the platform governor is in charge because the
    # restart budget was spent.
    wd_state_write "stopped" "$1 (last state: ${WD_LAST_WRITTEN:-none})"
    wd_lock_release
    exit 0
}

# ---------------------------------------------------------------- main loop

main() {
    if [ "${UPERF_WATCHDOG:-1}" = "0" ]; then
        echo "uperf: watchdog disabled (UPERF_WATCHDOG=0)"
        exit 0
    fi

    WD_BOOT_ID="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
    wd_stat_fields "$$"
    WD_SELF_START="$WD_START_TICKS"
    WD_OWNED=0
    WD_RESTARTS=0
    WD_OBSERVE_ONLY=0
    WD_MISS=0
    WD_STATE_CUR=""
    WD_LAST_WRITTEN=""
    WD_DRY_RUN=0
    WD_GAVE_UP=0
    [ "${UPERF_WATCHDOG_DRY_RUN:-0}" = "1" ] && WD_DRY_RUN=1
    [ "$WD_DRY_RUN" = "1" ] && WD_OBSERVE_ONLY=1
    WD_SUP=""
    WD_WORK=""
    WD_ALL=""
    WD_LAST_SUP=""
    WD_LAST_WORKERS="0"
    WD_LAST_ARMED=""

    wd_rotate_log

    if ! wd_lock_acquire; then
        wd_log "another watchdog owns the lock ($(wd_lock_owner)) — exiting"
        exit 0
    fi
    trap 'wd_shutdown "signalled"' TERM INT

    wd_log "watchdog started pid=$$ boot=$(echo "$WD_BOOT_ID" | cut -c1-8) interval=${WD_INTERVAL}s retry=${WD_RETRY_INTERVAL}s grace=${WD_GRACE} max_restarts=${WD_MAX_RESTARTS} dry_run=$WD_DRY_RUN exe=$DAEMON_EXE"
    wd_state_write "starting" "watchdog up, waiting for the daemon"

    # `uperf_start` returns once the daemon has forked and waited 2 s; give it a
    # moment so the first sample is not a false negative.
    wd_sleep 1

    while :; do
        if [ ! -f "$SCRIPT_PATH/libuperf.sh" ]; then
            wd_shutdown "module files are gone (uninstalled)"
        fi

        # One SF check per sample, and every branch below sleeps, so this stays a
        # per-sample cadence (off by default: one string test).
        wd_sf_check

        # Cache-first: the sweep below is the whole cost of a sample on a real device
        # (~0.19 s for 1289 pids, measured), and the answer it gives is unchanged while
        # the processes it found are still the same processes.
        WD_SINCE_SCAN=$((WD_SINCE_SCAN + 1))
        if [ "$WD_SINCE_SCAN" -ge "$WD_BACKSTOP" ] || ! wd_cache_fresh; then
            WD_SCANS=$((WD_SCANS + 1))
            WD_SINCE_SCAN=0
            wd_scan
            wd_cache_from_scan
        else
            wd_classify "$WD_CACHE"
        fi
        WD_LAST_SUP="${WD_SUP# }"
        set -- $WD_WORK
        WD_LAST_WORKERS=$#
        WD_LAST_ARMED="$(wd_armed_list)"
        set -- $WD_SUP
        sup_n=$#
        set -- $WD_WORK
        work_n=$#

        if [ "$sup_n" -eq 1 ] && [ "$work_n" -ge 1 ]; then
            # The log stays change-only (a line every 15 s would drown it), but the
            # *state file* is rewritten every sample: `updated_uptime_ms` is the only
            # evidence that a supervisor which has nothing to report is still alive
            # and still sampling (the device check for exactly that is what caught
            # this — a state file frozen at boot looks identical to a dead watchdog).
            if [ "$WD_STATE_CUR" != "running" ]; then
                wd_log "healthy: sup=[${WD_SUP# }] workers=[${WD_WORK# }] armed=[$WD_LAST_ARMED]"
                WD_STATE_CUR="running"
            fi
            wd_state_write "running" "supervisor + worker up"
            WD_MISS=0
            wd_sleep "$WD_INTERVAL"
        else
            WD_MISS=$((WD_MISS + 1))
            wd_log "unhealthy sample $WD_MISS/$WD_GRACE: sup_n=$sup_n work_n=$work_n armed=[$WD_LAST_ARMED]"
            WD_STATE_CUR="unhealthy"
            if [ "$WD_OBSERVE_ONLY" = "1" ]; then
                # Dry run (or a spent budget): report the verdict, change nothing.
                if [ "$WD_GAVE_UP" = "1" ]; then
                    wd_state_write "gave-up" "still unhealthy at sample $WD_MISS; platform governor in charge (budget spent)"
                else
                    wd_state_write "unhealthy" "sample $WD_MISS/$WD_GRACE (observe-only)"
                fi
                wd_sleep "$WD_RETRY_INTERVAL"
            elif [ "$WD_MISS" -ge "$WD_GRACE" ]; then
                WD_MISS=0
                wd_recover
                wd_sleep "$WD_RETRY_INTERVAL"
            else
                wd_sleep "$WD_RETRY_INTERVAL"
            fi
        fi
    done
}

# The test stub was sourced at the top, before almost every function existed. Sourcing it
# once more here - after every definition and just before the loop starts - is what makes
# "a test can replace any function" true rather than only the ones defined above it.
# (Nothing in a stub should have side effects for this to stay safe; they are pure
# definitions by construction.)
if [ -n "${UPERF_WATCHDOG_STUB:-}" ] && [ -f "$UPERF_WATCHDOG_STUB" ]; then
    . "$UPERF_WATCHDOG_STUB"
fi

main "$@"
