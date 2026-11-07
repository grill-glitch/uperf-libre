#!/system/bin/sh
#
# Copyright (C) 2023 Ham Jin & yinwanxi
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

# Runonce after boot, to speed up the transition of power modes in powercfg
BASEDIR="$(dirname "$(readlink -f "$0")")"
# Validate the includes: if one is missing (partial extraction, renamed file after an update)
# `mask_val`/`lock_val`/`is_mtk` become undefined, `is_mtk` reads as false, and the script
# falls into the Qualcomm branch emitting hundreds of "command not found" lines while the
# real configuration is silently not applied.
for _lib in pathinfo.sh libcommon.sh libpowercfg.sh libcgroup.sh libsysinfo.sh; do
    if [ ! -f "$BASEDIR/$_lib" ]; then
        echo "uperf: missing $_lib, skip platform tuning" >&2
        exit 1
    fi
    . "$BASEDIR/$_lib"
done
for _fn in mask_val lock_val lock is_mtk; do
    command -v "$_fn" >/dev/null 2>&1 || { echo "uperf: $_fn undefined, skip platform tuning" >&2; exit 1; }
done



# MTK specified
if [ "$(is_mtk)" = "true" ]; then
    # Also honour the module markers: a disabled or pending-removal module keeps its
    # directory, but its boost_affinity overrides are inactive — disabling the kernel-side
    # boost then leaves the device with no affinity boost at all.
    if [ -d "/data/adb/modules/asoul_affinity_opt" ] &&
        [ ! -f "/data/adb/modules/asoul_affinity_opt/disable" ] &&
        [ ! -f "/data/adb/modules/asoul_affinity_opt/remove" ]; then
        mask_val "0" /sys/module/mtk_fpsgo/parameters/boost_affinity
        mask_val "0" /sys/module/fbt_cpu/parameters/boost_affinity
        mask_val "0" /sys/kernel/fpsgo/minitop/enable
    else
        mask_val "1" /sys/module/mtk_fpsgo/parameters/boost_affinity
        mask_val "1" /sys/module/fbt_cpu/parameters/boost_affinity
        mask_val "1" /sys/kernel/fpsgo/minitop/enable
    fi
    mask_val "0" /sys/module/mtk_fpsgo/parameters/perfmgr_enable
    mask_val "0" /sys/module/mtk_core_ctl/parameters/policy_enable
    # FPSGO thermal
    mask_val "0" /sys/kernel/fpsgo/fbt/thrm_enable
    mask_val "95000" /sys/kernel/fpsgo/fbt/thrm_temp_th
    mask_val "-1" /sys/kernel/fpsgo/fbt/thrm_limit_cpu
    mask_val "-1" /sys/kernel/fpsgo/fbt/thrm_sub_cpu

    # Platform specified Config
    if [ -d "/proc/gpufreqv2" ]; then
        # Disabel auto voltage scaling by MTK
        lock_val "disable" /proc/gpufreqv2/aging_mode
        #Battery current limit
        lock_val "stop 1" /proc/mtk_batoc_throttling/battery_oc_protect_stop
        #echo "killing gpu thermal"
        for i in $(seq 0 10); do
            lock_val "$i 0 0" /proc/gpufreqv2/limit_table
        done
        lock_val "1 1 1" /proc/gpufreqv2/limit_table
        lock_val "3 1 1" /proc/gpufreqv2/limit_table
        lock_val "0" /sys/kernel/ged/hal/fastdvfs_mode
    else
        # Disabel auto voltage scaling by MTK
        lock_val "0" /proc/gpufreq/gpufreq_aging_enable
        # Enable CPU7 for MTK, MT6893 and before(need modify powerhal)
        mask_val "" /sys/devices/system/cpu/sched/cpu_prefer
        mask_val "" /sys/devices/system/cpu/sched/set_sched_isolation
        for i in $(seq 0 9); do
            mask_val "0" "$CPU"/cpu"$i"/sched_load_boost
            mask_val "$i" /sys/devices/system/cpu/sched/set_sched_deisolation
        done
        lock_val "0" /sys/devices/system/cpu/sched/hint_enable
        lock_val "65" /sys/devices/system/cpu/sched/hint_load_thresh
        # Force uperf to use PPM, using each cluster's real maximum: a hard-coded 3.2 GHz is
        # rejected on slower clusters and a non-zero *min* pins them until uperf takes over.
        echo "force uperf use PPM"
        for i in 0 1 2; do
            _max=""
            _max="$(cat "/sys/devices/system/cpu/cpufreq/policy$i/cpuinfo_max_freq" 2>/dev/null)"
            case "$_max" in '' | *[!0-9]*) continue ;; esac
            lock_val "$i $_max" /proc/ppm/policy/hard_userlimit_max_cpu_freq
            lock_val "$i 0" /proc/ppm/policy/hard_userlimit_min_cpu_freq
        done
        lock_val "0" /proc/ppm/cobra_limit_to_budget
        lock_val "0" /proc/ppm/cobra_budget_to_limit
        # Expand the glob here: `lock` does not glob its argument.
        for _f in /proc/ppm/policy/* /proc/ppm/*; do
            lock "$_f"
        done
        for i in $(seq 0 8); do
            lock_val "$i 0 0" /proc/gpufreq/gpufreq_limit_table
        done
        # MTK-EARA
        mask_val "0" /sys/kernel/eara_thermal/enable
    fi
else
    # "not MTK" is not "Qualcomm": `is_mtk` also returns false when detection failed, so
    # require the Qualcomm nodes themselves before applying kgsl/walt/metis tuning.
    if [ ! -d /sys/class/kgsl/kgsl-3d0 ]; then
        echo "uperf: neither MTK nor Qualcomm nodes found, skip platform tuning"
        exit 0
    fi
    BUS_DIR="/sys/devices/system/cpu/bus_dcvs"
    for d in "$BUS_DIR"/*; do
        [ -d "$d" ] || continue
        [ -f "$d/hw_max_freq" ] || continue
        MAX_FREQ="$(cat "$d/hw_max_freq" 2>/dev/null)"
        case "$MAX_FREQ" in '' | *[!0-9]*) continue ;; esac

        # Real sub-directories that actually carry max_freq; a bare `ls` also walked files
        # (hw_max_freq included) and an unreadable node wrote a bare newline into every
        # max_freq it did find.
        for df in "$d"/*/; do
            [ -f "$df/max_freq" ] || continue
            lock_val "$MAX_FREQ" "$df/max_freq"
        done
    done

    # Validate before use: a missing/empty/garbage node made this `-1` or an expansion
    # error, and an out-of-range level is rejected by kgsl (corrupting the GPU power level
    # on platforms where the nodes do exist).
    _num_pwrlvl="$(cat /sys/class/kgsl/kgsl-3d0/num_pwrlevels 2>/dev/null)"
    case "$_num_pwrlvl" in
        '' | *[!0-9]* | 0) MIN_PWRLVL="" ;;
        *) MIN_PWRLVL=$((_num_pwrlvl - 1)) ;;
    esac
    if [ -n "$MIN_PWRLVL" ]; then
        mask_val "$MIN_PWRLVL" /sys/class/kgsl/kgsl-3d0/default_pwrlevel
        mask_val "$MIN_PWRLVL" /sys/class/kgsl/kgsl-3d0/min_pwrlevel
    fi
    mask_val "0" /sys/class/kgsl/kgsl-3d0/thermal_pwrlevel
    mask_val "0" /sys/class/kgsl/kgsl-3d0/bus_split
    mask_val "0" /sys/class/kgsl/kgsl-3d0/force_bus_on
    mask_val "0" /sys/class/kgsl/kgsl-3d0/force_clk_on
    mask_val "0" /sys/class/kgsl/kgsl-3d0/force_no_nap
    mask_val "0" /sys/class/kgsl/kgsl-3d0/force_rail_on
    mask_val "0" /sys/class/kgsl/kgsl-3d0/throttling
    mask_val "0" /proc/sys/walt/sched_boost
    mask_val "0" /sys/module/metis/parameters/cluaff_control
    mask_val "0" /sys/module/metis/parameters/mi_fboost_enable
    mask_val "0" /sys/module/metis/parameters/mi_freq_enable
    mask_val "0" /sys/module/metis/parameters/mi_link_enable
    mask_val "0" /sys/module/metis/parameters/mi_switch_enable
    mask_val "0" /sys/module/metis/parameters/mi_viptask
    mask_val "0" /sys/module/metis/parameters/mpc_fboost_enable
    mask_val "0" /sys/module/metis/parameters/vip_link_enable
    mask_val "0" /sys/module/perfmgr/parameters/perfmgr_enable
fi

# OPLUS
mask_val "0" /sys/module/cpufreq_bouncing/parameters/enable
mask_val "0" /proc/task_info/task_sched_info/task_sched_info_enable
mask_val "0" /proc/oplus_scheduler/sched_assist/sched_assist_enabled
for i in 0 1 2;do
    mask_val "$i,0,5,3000,3,2000,3,2000" /sys/module/cpufreq_bouncing/parameters/config
done

# The last command's status is not this script's verdict: every step logs its own failure.
exit 0
