#!/system/bin/sh
#
# Copyright (C) 2021-2022 Matt Yang
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

BASEDIR="$(dirname "$0")"
. $BASEDIR/pathinfo.sh
. $BASEDIR/libcommon.sh
. $BASEDIR/libcgroup.sh

# The rewrite drives frequency through `scaling_governor=userspace` (the only
# writable path on a qcom-cpufreq-hw kernel — see docs/m5-cpu-governor.md §2), which
# is a *takeover*: if the daemon dies without disarming, the policies stay pinned at
# the last published frequency. The daemon restores them on every stop path it can
# run (including SIGTERM/SIGINT — see docs/m6b-evidence.md §9), but a SIGKILL leaves
# nothing able to do it. This is the script-side safety net, and it only ever undoes
# our own state: a policy is touched solely while it reads `userspace`.
GOVERNOR_STATE="$USER_PATH/orig_governor.txt"

# M9: external watchdog. `UPERF_CPUFREQ_ROOT` is the one definition of where the
# policies live, shared by the watchdog and the host harness
# (`scripts/test_watchdog_host.sh`) — same idea as `UPERF_FAKE_ROOT` on the Rust
# side, so the policy is exercisable without a phone.
WATCHDOG_SCRIPT="$SCRIPT_PATH/uperf_watchdog.sh"

# Late-bound on purpose: `FLAG_PATH` comes from pathinfo.sh, but a caller (the
# host harness) may redirect it after sourcing this file, and a lock path frozen
# at source time would then point at the real device module directory.
uperf_watchdog_lock_dir() {
    echo "$FLAG_PATH/uperf_watchdog.lock"
}

uperf_policy_dirs() {
    for d in "${UPERF_CPUFREQ_ROOT:-/sys/devices/system/cpu/cpufreq}"/policy*; do
        [ -d "$d" ] && [ -f "$d/scaling_governor" ] && echo "$d"
    done
}

# Remember the governors in effect *before* we take over. A policy already in
# `userspace` is our own state, so it is never recorded — that keeps a restart from
# overwriting the real original with `userspace`.
uperf_save_governors() {
    mkdir -p "$USER_PATH"
    local tmp="$GOVERNOR_STATE.new"
    : >"$tmp"
    for d in $(uperf_policy_dirs); do
        local cur
        cur="$(cat $d/scaling_governor 2>/dev/null)"
        [ -z "$cur" ] && continue
        [ "$cur" = "userspace" ] && continue
        echo "$(basename $d) $cur" >>"$tmp"
    done
    [ -s "$tmp" ] && mv "$tmp" "$GOVERNOR_STATE" || rm -f "$tmp"
}

# Undo the takeover, but only where the takeover is visible AND we know what to
# restore. Never invent a governor: `powersave` means "always the lowest
# frequency", so guessing it would be worse than leaving the policy alone. The
# daemon records the originals at the same path when it takes over
# (`orig_governor.txt`, `<policy> <governor>` lines), and uperf_start records them
# even earlier.
uperf_restore_governors() {
    local restored=0 skipped=0
    for d in $(uperf_policy_dirs); do
        [ "$(cat $d/scaling_governor 2>/dev/null)" = "userspace" ] || continue
        local name want=""
        name="$(basename $d)"
        if [ -f "$GOVERNOR_STATE" ]; then
            want="$(grep "^$name " "$GOVERNOR_STATE" 2>/dev/null | awk '{print $2}')"
        fi
        if [ -z "$want" ] || [ "$want" = "userspace" ]; then
            echo "uperf: no recorded original governor for $name, leaving it untouched"
            skipped=$((skipped+1))
            continue
        fi
        echo "$want" >$d/scaling_governor 2>/dev/null && restored=$((restored+1))
    done
    [ "$restored" -gt 0 ] && echo "uperf: restored $restored cpu governor(s)"
    [ "$skipped" -gt 0 ] && echo "uperf: $skipped policy(ies) left as-is (no recorded original)"
    return 0
}

# The daemon's write ledger (`<USER_PATH>/sysfs_orig.txt`, written by
# `rust/uperf-core/src/sysfs_ledger.rs`): the value each sysfs knob held before the
# daemon's *first* write to it. Same contract as the governors above — a path with no
# recorded original is left alone and reported, never invented.
#
# Why the restore lives here and not in the daemon: the interesting case is the daemon
# that no longer exists. `/proc/<pid>/exe` has moved on; this file has not.
SYSFS_ORIG="${UPERF_SYSFS_ORIG:-$USER_PATH/sysfs_orig.txt}"
# Prefix for the paths in the ledger. Empty on a device (the ledger holds real paths);
# set by the host/device harnesses so a restore never touches the real /sys.
UPERF_SYSFS_ROOT="${UPERF_SYSFS_ROOT:-}"

uperf_restore_sysfs() {
    local restored=0 unknown=0 failed=0 kept=0 corrupt=0 p v target cur
    if [ ! -f "$SYSFS_ORIG" ]; then
        return 0
    fi
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
        ''|'#'*) continue ;;
        esac
        # Absolute paths only, exactly like the Rust loader this reads after. A line
        # that is not one is *not* a path we can act on: skipping it silently would
        # hide a hand-edited or truncated file, and treating it as a path would write
        # somewhere the ledger never named (caught by the harness: `garbage with no
        # path` became a write to `<root>garbage`).
        case "$line" in
        /*) ;;
        *)
            echo "uperf: ignoring unreadable line in $SYSFS_ORIG: $line"
            corrupt=$((corrupt + 1))
            continue
            ;;
        esac
        # `<path> <value>`, or a bare `<path>` for "written, no readable original".
        # Paths never contain whitespace (the config's knob table is the only source,
        # and `every_knob_path_is_whitespace_free` keeps it that way), so the first
        # space ends the path and the rest — spaces included — is the value.
        p="${line%% *}"
        if [ "$p" = "$line" ]; then
            v=""
        else
            v="${line#* }"
        fi
        if [ -z "$v" ]; then
            echo "uperf: no recorded original for $p, leaving it untouched"
            unknown=$((unknown + 1))
            continue
        fi
        target="$UPERF_SYSFS_ROOT$p"
        cur=""
        IFS= read -r cur <"$target" 2>/dev/null
        [ "$cur" = "$v" ] && { kept=$((kept + 1)); continue; }
        if printf '%s' "$v" >"$target" 2>/dev/null; then
            restored=$((restored + 1))
        else
            echo "uperf: could not restore $p (write refused), ledger kept"
            failed=$((failed + 1))
        fi
    done <"$SYSFS_ORIG"

    [ "$restored" -gt 0 ] && echo "uperf: restored $restored sysfs knob(s)"
    [ "$kept" -gt 0 ] && echo "uperf: $kept sysfs knob(s) already at their original value"
    [ "$unknown" -gt 0 ] && echo "uperf: $unknown sysfs knob(s) left as-is (no recorded original)"
    [ "$corrupt" -gt 0 ] && echo "uperf: $corrupt unreadable ledger line(s) ignored"
    # Cleared only when nothing is owed: an entry we could not put back has to survive
    # for the next attempt (a later stop, or the watchdog's dead-man path).
    if [ "$unknown" -eq 0 ] && [ "$failed" -eq 0 ] && [ "$corrupt" -eq 0 ]; then
        rm -f "$SYSFS_ORIG" 2>/dev/null
    else
        [ "$failed" -gt 0 ] && echo "uperf: sysfs ledger kept at $SYSFS_ORIG"
    fi
    return 0
}

# ---------------------------------------------------------------- M9 watchdog
#
# The external supervisor for the daemon. Why a *script* and not another thread:
# the two exits that leave the CPU pinned in `userspace` (SIGKILL, and a Rust
# panic under `panic = "abort"`) are exactly the ones no in-process path survives,
# so the reaction has to come from outside the process. See
# `magisk/script/uperf_watchdog.sh` and `docs/m9-watchdog.md`.

uperf_watchdog_pid() {
    local owner lock_dir
    lock_dir="$(uperf_watchdog_lock_dir)"
    [ -f "$lock_dir/owner" ] || return 0
    owner="$(cat "$lock_dir/owner" 2>/dev/null)"
    [ -n "$owner" ] || return 0
    echo "${owner%%:*}"
}

# Stopped by `uperf_stop` and by the uninstaller, so an explicit stop is never
# fought by a supervisor that is still convinced the daemon should be up.
uperf_watchdog_stop() {
    local pid lock_dir i
    pid="$(uperf_watchdog_pid)"
    [ -n "$pid" ] || return 0
    kill -TERM "$pid" 2>/dev/null
    i=0
    while [ "$i" -lt 5 ]; do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
        i=$((i + 1))
    done
    # A watchdog that did not exit in time still holds the lock; the caller is
    # stopping the module, so drop it and let the process die on its own.
    lock_dir="$(uperf_watchdog_lock_dir)"
    rm -rf "$lock_dir" 2>/dev/null
    return 0
}

uperf_watchdog_start() {
    [ "${UPERF_WATCHDOG:-1}" = "0" ] && return 0
    [ "$UPERF_WATCHDOG_SUPPRESS" = "1" ] && return 0
    [ -f "$WATCHDOG_SCRIPT" ] || {
        echo "uperf: watchdog script missing ($WATCHDOG_SCRIPT), skipping"
        return 0
    }
    # `setsid` where available: the watchdog must outlive the shell that started it
    # (at boot that is service.sh, over adb or the WebUI a short-lived pipe).
    local runner="sh"
    command -v setsid >/dev/null 2>&1 && runner="setsid sh"
    # shellcheck disable=SC2086 # intentional word splitting of "$runner"
    $runner "$WATCHDOG_SCRIPT" </dev/null >/dev/null 2>&1 &
    return 0
}

uperf_stop() {
    # Stop the watchdog first: it would otherwise treat this stop as a crash and
    # restart the daemon we are trying to take down.
    uperf_watchdog_stop
    killall uperf
    # give the daemon its chance to disarm gracefully, then make sure
    sleep 1
    uperf_restore_governors
    uperf_restore_sysfs
}

# Keep the previous run's log, but bounded. `uperf_start` moves the log aside on
# every start, and nothing ever trimmed the backup: measured on alioth, 138 MB of
# `/sdcard` held by `uperf_log.txt.bak`. Trimming a file that size on /sdcard at boot
# would cost more than the space it frees, so an oversized backup is dropped instead
# of read. The current log is bounded by the daemon itself (spdlog's rotating sink,
# `UPERF_LOG_MAX_BYTES`); this only covers the copy the script makes.
UPERF_LOG_BACKUP_MAX_BYTES="${UPERF_LOG_BACKUP_MAX_BYTES:-16777216}"

uperf_trim_log_backup() {
    local bak="$USER_PATH/uperf_log.txt.bak" size
    [ -f "$bak" ] || return 0
    size="$(wc -c <"$bak" 2>/dev/null)"
    case "$size" in
    '' | *[!0-9]*) return 0 ;;
    esac
    [ "$size" -le "$UPERF_LOG_BACKUP_MAX_BYTES" ] && return 0
    rm -f "$bak" 2>/dev/null
    echo "uperf: dropped an oversized log backup ($size > $UPERF_LOG_BACKUP_MAX_BYTES bytes)"
    return 0
}

# The user directory is seeded exactly once, at install time (`script/setup.sh`),
# and the installed module deliberately keeps no copy of `config/` (setup.sh removes
# it). Nothing else recreates it — so if the directory is ever lost, the daemon starts
# with no config, logs `Config file not found`, and the module sits there doing nothing
# forever, with no way back short of a reinstall. Measured on alioth: exactly that,
# after the user directory was removed. Keep a pristine copy at install time and
# restore it here, loudly, so the failure is recoverable without a reinstall.
uperf_ensure_config() {
    mkdir -p "$USER_PATH" 2>/dev/null
    # /sdcard is an emulated view: creating the directory through it is what the
    # module's own paths need, and it is also what keeps the view consistent.
    [ -d "$USER_PATH" ] || mkdir -p /data/media/0/Android/yc/uperf 2>/dev/null
    [ -f "$USER_PATH/uperf.json" ] && return 0
    if [ -f "$USER_PATH/uperf.json.default" ]; then
        cp -f "$USER_PATH/uperf.json.default" "$USER_PATH/uperf.json"
        echo "uperf: no config at $USER_PATH/uperf.json, restored the installed default"
        return 0
    fi
    echo "uperf: no config and no installed default to restore — reinstall the module"
    return 1
}

uperf_start() {
    uperf_ensure_config

    # A previous run may have died without disarming (SIGKILL). Undo that first,
    # then record the originals we are about to replace.
    uperf_restore_governors
    uperf_restore_sysfs
    uperf_save_governors

    # raise inotify limit in case file sync existed
    lock_val "1048576" /proc/sys/fs/inotify/max_queued_events
    lock_val "1048576" /proc/sys/fs/inotify/max_user_watches
    lock_val "1024" /proc/sys/fs/inotify/max_user_instances

    mv $USER_PATH/uperf_log.txt $USER_PATH/uperf_log.txt.bak
    uperf_trim_log_backup
    if [ -f $BIN_PATH/libc++_shared.so ]; then
        ASAN_LIB="$(ls $BIN_PATH/libclang_rt.asan-*-android.so)"
        export LD_PRELOAD="$ASAN_LIB $BIN_PATH/libc++_shared.so"
    fi
    # Detach the daemon's stdio from whoever called us.
    #
    # The log file is the daemon's interface, but spdlog's default logger writes to
    # stdout as well, and at this point stdout is the caller's pipe: an interactive
    # shell at boot, or the WebUI's `exec`, whose read end disappears the moment the
    # command returns. A write to that broken pipe raises SIGPIPE and took the daemon
    # down ~90s after a WebUI-style restart (see app_main.cpp and
    # docs/m7-evidence.md §7). The daemon also ignores SIGPIPE; this keeps the failure
    # impossible rather than merely survivable, and keeps `-o` the only sink.
    #
    # `setsid()` inside the daemon is what makes it outlive this script; the
    # redirections below only concern stdio.
    $BIN_PATH/uperf $USER_PATH/uperf.json -o $USER_PATH/uperf_log.txt \
        </dev/null >/dev/null 2>&1

    # waiting for uperf initialization
    sleep 2
    # uperf shouldn't preempt foreground tasks
    rebuild_process_scan_cache
    change_task_cgroup "uperf" "background" "cpuset"

    # M9: hand supervision to the watchdog. Started last, so it never observes the
    # 2 s window above as a crash; a second instance exits on the owner lock.
    uperf_watchdog_start
}
