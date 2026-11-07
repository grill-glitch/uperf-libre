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

# process scan cache
ps_ret=""

# launcher&home usually in foreground cpuset group
# reserve one LITTLE for hwservicemanager which will block composer
# thread priority settings not working on hwservicemanager
# okay, disabling perf-hal resulting hwservicemanager frequently wakeup
# CPUMASK_LOW="0e"
# CPUMASK_MID="73"
# CPUMASK_HIGH="f0"
# CPUID_LOW="1-3"
# CPUID_MID="0-1,4-7" # uperf set 0-1/0-6 for foreground, cpu7 reserved for launcher
# CPUID_HIGH="4-7"

# $1:task_name $2:cgroup_name $3:"cpuset"/"stune"
change_task_cgroup() {
    local comm
    # An empty pattern matches every process: refuse it instead of moving the whole system.
    [ -n "$1" ] || return 1
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        # Glob, not `ls`: one fork per matched pid just to list a directory. A vanished pid
        # (the ps snapshot is stale) is skipped instead of printing "No such file".
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            echo "$temp_tid" >"$(_cgroup_tasks_path "$3" "$2")"
        done
    done
}

# $1:process_name $2:cgroup_name $3:"cpuset"/"stune"
change_proc_cgroup() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        echo "$temp_pid" >"/dev/$3/$2/cgroup.procs" 2>/dev/null
    done
}

# $1:task_name $2:thread_name $3:cgroup_name $4:"cpuset"/"stune"
change_thread_cgroup() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        # An empty thread name matches every thread: require a pattern.
        [ -n "$2" ] || return 1
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            comm="$(cat "/proc/$temp_pid/task/$temp_tid/comm" 2>/dev/null)"
            if printf '%s\n' "$comm" | grep -qiE -- "$2"; then
                echo "$temp_tid" >"/dev/$4/$3/tasks"
            fi
        done
    done
}

# $1:task_name $2:cgroup_name $3:"cpuset"/"stune"
change_main_thread_cgroup() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        comm="$(cat /proc/$temp_pid/comm)"
        echo $temp_pid >"/dev/$3/$2/tasks"
    done
}

# $1:task_name $2:hex_mask(0x00000003 is CPU0 and CPU1)
change_task_affinity() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            taskset -p "$2" "$temp_tid" >>"${LOG_FILE:-/dev/null}" 2>&1 || echo "libcgroup: taskset $2 on tid $temp_tid failed" >>"${LOG_FILE:-/dev/stderr}"
        done
    done
}

# $1:task_name $2:thread_name $3:hex_mask(0x00000003 is CPU0 and CPU1)
change_thread_affinity() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -n "$2" ] || return 1
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            comm="$(cat "/proc/$temp_pid/task/$temp_tid/comm" 2>/dev/null)"
            if printf '%s\n' "$comm" | grep -qiE -- "$2"; then
                taskset -p "$3" "$temp_tid" >>"${LOG_FILE:-/dev/null}"
            fi
        done
    done
}

# $1:task_name $2:nice(relative to 120)
change_task_nice() {
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            # Validate first and check each step: a failure left the thread at nice -19
            # (maximum priority) on an arbitrary matched system thread.
            case "$2" in '' | *[!0-9-]*) continue ;; esac
            renice -n +40 -p "$temp_tid" </dev/null >/dev/null 2>&1 || continue
            renice -n -19 -p "$temp_tid" </dev/null >/dev/null 2>&1 || continue
            renice -n "$2" -p "$temp_tid" </dev/null >>"${LOG_FILE:-/dev/null}" 2>&1 ||
                renice -n 0 -p "$temp_tid" </dev/null >/dev/null 2>&1
        done
    done
}

# $1:task_name $2:thread_name $3:nice(relative to 120)
change_thread_nice() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -n "$2" ] || return 1
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            comm="$(cat "/proc/$temp_pid/task/$temp_tid/comm" 2>/dev/null)"
            if printf '%s\n' "$comm" | grep -qiE -- "$2"; then
                renice -n +40 -p "$temp_tid"
                renice -n -19 -p "$temp_tid"
                renice -n "$3" -p "$temp_tid"
            fi
        done
    done
}

# $1:task_name $2:priority(99-x, 1<=x<=99)
change_task_rt() {
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            chrt -f -p "$2" "$temp_tid" >>"${LOG_FILE:-/dev/null}"
        done
    done
}

# $1:task_name $2:thread_name $3:priority(99-x, 1<=x<=99)
change_thread_rt() {
    local comm
    for temp_pid in $(echo "$ps_ret" | grep -i -E "$1" | awk '{print $1}'); do
        [ -n "$2" ] || return 1
        [ -d "/proc/$temp_pid/task" ] || continue
        for temp_tid in /proc/$temp_pid/task/*; do
            temp_tid="${temp_tid##*/}"
            comm="$(cat "/proc/$temp_pid/task/$temp_tid/comm" 2>/dev/null)"
            if printf '%s\n' "$comm" | grep -qiE -- "$2"; then
                chrt -f -p "$3" "$temp_tid" >>"${LOG_FILE:-/dev/null}"
            fi
        done
    done
}

# $1:task_name
change_task_high_prio() {
    # audio thread nice <= -16
    change_task_nice "$1" "-15"
}

# $1:task_name $2:thread_name
change_thread_high_prio() {
    # audio thread nice <= -16
    change_thread_nice "$1" "$2" "-15"
}

# $1:task_name $2:thread_name
unpin_thread() {
    change_thread_cgroup "$1" "$2" "" "cpuset"
}

# $1:task_name $2:thread_name
pin_thread_on_pwr() {
    change_thread_cgroup "$1" "$2" "background" "cpuset"
}

# $1:task_name $2:thread_name
pin_thread_on_mid() {
    unpin_thread "$1" "$2"
    change_thread_affinity "$1" "$2" "7f"
}

# $1:task_name $2:thread_name
pin_thread_on_perf() {
    unpin_thread "$1" "$2"
    change_thread_affinity "$1" "$2" "f0"
}

# $1:task_name
unpin_proc() {
    change_task_cgroup "$1" "" "cpuset"
}

# $1:task_name
pin_proc_on_pwr() {
    change_task_cgroup "$1" "background" "cpuset"
}

# $1:task_name
pin_proc_on_mid() {
    unpin_proc "$1"
    change_task_affinity "$1" "7f"
}

# $1:task_name
pin_proc_on_perf() {
    unpin_proc "$1"
    change_task_affinity "$1" "f0"
}

rebuild_process_scan_cache() {
    # avoid matching grep itself
    # ps -Ao pid,args | grep kswapd
    # 150 [kswapd0]
    # 16490 grep kswapd
    ps_ret="$(ps -Ao pid,args)"
}
