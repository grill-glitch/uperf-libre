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

uperf_policy_dirs() {
    for d in /sys/devices/system/cpu/cpufreq/policy*; do
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

uperf_stop() {
    killall uperf
    # give the daemon its chance to disarm gracefully, then make sure
    sleep 1
    uperf_restore_governors
}

uperf_start() {
    # A previous run may have died without disarming (SIGKILL). Undo that first,
    # then record the originals we are about to replace.
    uperf_restore_governors
    uperf_save_governors

    # raise inotify limit in case file sync existed
    lock_val "1048576" /proc/sys/fs/inotify/max_queued_events
    lock_val "1048576" /proc/sys/fs/inotify/max_user_watches
    lock_val "1024" /proc/sys/fs/inotify/max_user_instances

    mv $USER_PATH/uperf_log.txt $USER_PATH/uperf_log.txt.bak
    if [ -f $BIN_PATH/libc++_shared.so ]; then
        ASAN_LIB="$(ls $BIN_PATH/libclang_rt.asan-*-android.so)"
        export LD_PRELOAD="$ASAN_LIB $BIN_PATH/libc++_shared.so"
    fi
    $BIN_PATH/uperf $USER_PATH/uperf.json -o $USER_PATH/uperf_log.txt

    # waiting for uperf initialization
    sleep 2
    # uperf shouldn't preempt foreground tasks
    rebuild_process_scan_cache
    change_task_cgroup "uperf" "background" "cpuset"
}
