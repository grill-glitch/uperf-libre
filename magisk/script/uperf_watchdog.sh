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
# Test seams (all default to the device layout): UPERF_WATCHDOG_{PROC_ROOT,
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
WD_LOG="${UPERF_WATCHDOG_LOG:-$USER_PATH/uperf_watchdog.log}"
WD_STATE="${UPERF_WATCHDOG_STATE:-$USER_PATH/uperf_watchdog.state}"
WD_LOCK_DIR="$FLAG_PATH/uperf_watchdog.lock"
WD_INTERVAL="${UPERF_WATCHDOG_INTERVAL:-15}"
WD_RETRY_INTERVAL="${UPERF_WATCHDOG_RETRY_INTERVAL:-3}"
WD_GRACE="${UPERF_WATCHDOG_GRACE:-2}"
WD_MAX_RESTARTS="${UPERF_WATCHDOG_MAX_RESTARTS:-3}"
WD_TEARDOWN_TICKS="${UPERF_WATCHDOG_TEARDOWN_TICKS:-5}"
WD_VERIFY_WAIT="${UPERF_WATCHDOG_VERIFY_WAIT:-3}"

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

# Field $2 of `/proc/<pid>/stat`, counted *after* the `(comm)` field — which may
# itself contain spaces and parentheses — so a plain `awk '{print $N}'` is wrong.
# After `(comm) `: 1 state, 2 ppid, ..., 20 starttime (field 22 overall).
wd_stat_field() {
    sed -n 's/^.*) //p' "$PROC_ROOT/$1/stat" 2>/dev/null | awk -v i="$2" '{print $i}'
}

wd_ppid() { wd_stat_field "$1" 2; }

wd_start_ticks() { wd_stat_field "$1" 20; }

# The module binary's image. `exe` survives the cmdline rewrite both of our
# processes go through, which is why identity is taken from here and not from the
# process name. A module update replaces the file under a running process, so the
# ` (deleted)` suffix readlink then reports is stripped.
wd_exe() {
    readlink "$PROC_ROOT/$1/exe" 2>/dev/null | sed 's/ (deleted)$//'
}

# Classify every process running $DAEMON_EXE.
#   WD_SUP  supervisor: no parent inside the set (dfps' daemon, setsid'ed)
#   WD_WORK its workers (the forked app that owns the governor)
#   WD_ALL  both, for teardown
wd_scan() {
    local p pid exe ppid
    WD_ALL=""
    for p in "$PROC_ROOT"/[0-9]*; do
        [ -d "$p" ] || continue
        pid="${p##*/}"
        exe="$(wd_exe "$pid")"
        [ "$exe" = "$DAEMON_EXE" ] || continue
        WD_ALL="$WD_ALL $pid"
    done

    WD_SUP=""
    WD_WORK=""
    for pid in $WD_ALL; do
        ppid="$(wd_ppid "$pid")"
        case " $WD_ALL " in
        *" $ppid "*) WD_WORK="$WD_WORK $pid" ;;
        *) WD_SUP="$WD_SUP $pid" ;;
        esac
    done
    return 0
}

# Policies currently reading `userspace` — the live sysfs value. This, not the
# status file and not a recorded value, is the authority on whether a takeover is
# in effect.
wd_armed() {
    local d
    for d in "$CPUFREQ_ROOT"/policy*; do
        [ -f "$d/scaling_governor" ] || continue
        [ "$(cat "$d/scaling_governor" 2>/dev/null)" = "userspace" ] || continue
        echo "${d##*/}"
    done
}

wd_armed_list() { wd_armed | tr '\n' ' '; }

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
    cur="$(wd_start_ticks "$pid")"
    [ -n "$cur" ] || return 1
    [ "$cur" = "$start" ] || return 1
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
    out="$(uperf_restore_governors 2>&1)"
    [ -n "$out" ] && wd_log "restore: $out"
    return 0
}

wd_recover() {
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
    WD_SELF_START="$(wd_start_ticks $$)"
    WD_OWNED=0
    WD_RESTARTS=0
    WD_OBSERVE_ONLY=0
    WD_MISS=0
    WD_STATE_CUR=""
    WD_LAST_WRITTEN=""
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

    wd_log "watchdog started pid=$$ boot=$(echo "$WD_BOOT_ID" | cut -c1-8) interval=${WD_INTERVAL}s retry=${WD_RETRY_INTERVAL}s grace=${WD_GRACE} max_restarts=${WD_MAX_RESTARTS} exe=$DAEMON_EXE"
    wd_state_write "starting" "watchdog up, waiting for the daemon"

    # `uperf_start` returns once the daemon has forked and waited 2 s; give it a
    # moment so the first sample is not a false negative.
    wd_sleep 1

    while :; do
        if [ ! -f "$SCRIPT_PATH/libuperf.sh" ]; then
            wd_shutdown "module files are gone (uninstalled)"
        fi

        wd_scan
        WD_LAST_SUP="${WD_SUP# }"
        set -- $WD_WORK
        WD_LAST_WORKERS=$#
        WD_LAST_ARMED="$(wd_armed_list)"
        set -- $WD_SUP
        sup_n=$#
        set -- $WD_WORK
        work_n=$#

        if [ "$sup_n" -eq 1 ] && [ "$work_n" -ge 1 ]; then
            if [ "$WD_STATE_CUR" != "running" ]; then
                wd_log "healthy: sup=[${WD_SUP# }] workers=[${WD_WORK# }] armed=[$WD_LAST_ARMED]"
                wd_state_write "running" "supervisor + worker up"
                WD_STATE_CUR="running"
            fi
            WD_MISS=0
            wd_sleep "$WD_INTERVAL"
        else
            WD_MISS=$((WD_MISS + 1))
            wd_log "unhealthy sample $WD_MISS/$WD_GRACE: sup_n=$sup_n work_n=$work_n armed=[$WD_LAST_ARMED]"
            WD_STATE_CUR="unhealthy"
            if [ "$WD_MISS" -ge "$WD_GRACE" ] && [ "$WD_OBSERVE_ONLY" = "0" ]; then
                WD_MISS=0
                wd_recover
                wd_sleep "$WD_RETRY_INTERVAL"
            else
                wd_sleep "$WD_RETRY_INTERVAL"
            fi
        fi
    done
}

main "$@"
