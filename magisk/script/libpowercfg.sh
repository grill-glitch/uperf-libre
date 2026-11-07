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

###############################
# Powermodes helper functions
###############################

# $1:keyword $2:nr_max_matched
get_package_name_by_keyword() {
    # Validate the count (`head -n ""` fails silently), treat the keyword literally (`-F`,
    # `--` so a leading `-` is not an option, no regex metacharacters) and drop the redundant
    # `echo "$(...)"` round-trip.
    case "$2" in '' | *[!0-9]*) return 1 ;; esac
    pm list package 2>/dev/null | grep -F -- "$1" | head -n "$2" | cut -d: -f2
}

# $1:"0:576000 4:710400 7:825600"
set_cpufreq_min() {
    [ -n "$1" ] || return 0
    lock_val "$1" /sys/module/msm_performance/parameters/cpu_min_freq
    local key
    local val
    for kv in $1; do
        # An entry without `:` (or with an empty value) used to write a bogus value into a
        # non-existent path; `$1` unquoted would also glob.
        case "$kv" in
            *:*[!0-9]* | *:* | :* | *[!0-9]*:*) echo "uperf: bad cpu freq entry '$kv'" >&2; continue ;;
        esac
        key=${kv%:*}
        val=${kv#*:}
        lock_val "$val" /sys/devices/system/cpu/cpu$key/cpufreq/scaling_min_freq
    done
}

# $1:"0:576000 4:710400 7:825600"
set_cpufreq_max() {
    lock_val "$1" /sys/module/msm_performance/parameters/cpu_max_freq
    local key
    local val
    for kv in $1; do
        key=${kv%:*}
        val=${kv#*:}
        lock_val "$val" /sys/devices/system/cpu/cpu$key/cpufreq/scaling_max_freq
    done
}

# $1:"schedutil/pl" $2:"0:4 4:3 7:1"
set_governor_param() {
    local key
    local val
    # The HMP fallback node is *global*: writing it once per entry left only the last value,
    # so on a single-policy kernel every cluster but the last got the wrong parameter. Apply
    # the global node once, with the first (small-cluster) value, as upstream intends.
    local hmp_done=0
    for kv in $2; do
        key=${kv%:*}
        val=${kv#*:}
        lock_val "$val" /sys/devices/system/cpu/cpu$key/cpufreq/$1
        if [ "$hmp_done" = "0" ]; then
            lock_val "$val" /sys/devices/system/cpu/cpufreq/$1
            hmp_done=1
        fi
    done
}

# $1:"min_cpus" $2:"0:4 4:3 7:1"
set_corectl_param() {
    local key
    local val
    for kv in $2; do
        key=${kv%:*}
        val=${kv#*:}
        lock_val "$val" /sys/devices/system/cpu/cpu$key/core_ctl/$1
    done
}

# stop before updating cfg
perfhal_stop() {
    # Record which services were actually up, so `perfhal_start` only revives those.
    PERFHAL_WAS_RUNNING=""
    local s
    for i in 0 1 2 3 4; do
        for j in 0 1 2 3 4; do
            s="perf-hal-$i-$j"
            case "$(getprop "init.svc.$s" 2>/dev/null)" in
                running | starting) PERFHAL_WAS_RUNNING="$PERFHAL_WAS_RUNNING $s" ;;
            esac
            stop "$s" 2>/dev/null
        done
    done
    # Poll until the services are actually stopped: a fixed 500 us is not a synchronisation
    # point (25 services take longer), and the following sysfs writes would race a HAL that
    # re-applies its own policy.
    local i=0 svc state
    while [ "$i" -lt 40 ]; do
        local busy=0
        for svc in perf-hal-0-0 perf-hal-1-0 perf-hal-2-0 perf-hal-3-0 perf-hal-4-0; do
            state="$(getprop "init.svc.$svc" 2>/dev/null)"
            case "$state" in running | starting) busy=1 ;; esac
        done
        [ "$busy" = "0" ] && return 0
        usleep 25000
        i=$((i + 1))
    done
}

# start after updating cfg
perfhal_start() {
    # Only restart the services that were running before `perfhal_stop` recorded them: blindly
    # starting all 25 resurrects services the user/system deliberately stopped, and a HAL that
    # fails to start left the power mode applied with no HAL behind it.
    local svc
    if [ -n "$PERFHAL_WAS_RUNNING" ]; then
        for svc in $PERFHAL_WAS_RUNNING; do
            start "$svc" 2>/dev/null
        done
        return 0
    fi
    for i in 0 1 2 3 4; do
        for j in 0 1 2 3 4; do
            start "perf-hal-$i-$j" 2>/dev/null
        done
    done
}
