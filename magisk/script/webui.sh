#!/system/bin/sh
#
# Copyright (C) 2021-2022 Matt Yang
# Copyright (C) 2026 grill-glitch
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
# Control entry for the KernelSU WebUI (and for adb, which is how it is tested).
#
# The WebUI itself stays thin: it parses the config JSON in JS (it has a real JSON
# parser and the daemon reads the same file) and validates the user's intent; the
# actual reads/writes of device state happen here, in one place that can be exercised
# from a shell without the manager. Every subcommand prints `key=value` lines so the
# frontend never has to parse prose, and the *result* of a write is always read back
# from the file rather than assumed.

BASEDIR="$(cd "$(dirname "$0")" && pwd)"
. "$BASEDIR/pathinfo.sh"
. "$BASEDIR/libcommon.sh"

# libuperf.sh brings in `uperf_stop` / `uperf_start` and the governor bookkeeping.
# It resolves its own directory through `$0`, which is this script's path here, so it
# is sourced explicitly to keep the two independent of the caller's cwd.
. "$BASEDIR/libuperf.sh"

# Host-test seams, defaulting to the device paths (`scripts/test_watchdog_host.sh`
# exercises `status` without a phone). Placed before GOVERNOR_STATE/PRESET_FILE so
# everything derived from them follows the override too.
USER_PATH="${UPERF_WEBUI_USER_PATH:-$USER_PATH}"
FLAG_PATH="${UPERF_WEBUI_FLAG_PATH:-$FLAG_PATH}"

GOVERNOR_STATE="$USER_PATH/orig_governor.txt"
PRESET_FILE="$USER_PATH/cur_powermode.txt"

# `auto` is a legal value of cur_powermode.txt but is not a preset name — see
# rust/uperf-config/src/switcher.rs. Kept here so a bad WebUI input cannot write a
# value the daemon would have to reject.
PRESET_LEGAL_EXTRA="auto"

# `pathinfo.sh` prepends the module's private busybox to PATH, and
# `busybox --install -s` declares a `ps` applet there — but that `ps` rejects
# `-o PID,STAT,NAME` with "bad -o argument 'PID'", so a PATH-resolved `ps` prints
# nothing and a running daemon reads as `daemon.count=0` (which also made
# `restart.ok` report failure for a restart that worked). Always use the system `ps`.
SYS_PS=/system/bin/ps
[ -x "$SYS_PS" ] || SYS_PS=ps

# Both the daemon and its worker carry the name `uperf` (the daemon rewrites its
# argv), so both are counted. Match on the *last* column instead of a fixed column
# position: toybox `ps -A -o PID,STAT,NAME` prints ` 9837 Ss    uperf`, and a STAT of
# two characters (`Ss`) silently broke the earlier positional pattern — which read a
# live daemon as `daemon.count=0` and made `restart.ok` a lie. Zombies are excluded:
# a killed worker lingers as `[uperf]`/state `Z` and must not count as running.
uperf_pids() {
    $SYS_PS -A -o PID,STAT,NAME 2>/dev/null | awk '$NF == "uperf" && $2 !~ /Z/ { print $1 }'
}

uperf_pid_count() {
    uperf_pids | wc -l | tr -d ' '
}

uperf_pid_list() {
    uperf_pids | tr '\n' ' '
}

print_status() {
    local cfg="$USER_PATH/uperf.json"
    local pids; pids="$(uperf_pid_list)"
    echo "daemon.count=$(uperf_pid_count)"
    echo "daemon.pids=$pids"
    echo "user.path=$USER_PATH"
    echo "config.path=$cfg"
    echo "config.present=$([ -f "$cfg" ] && echo 1 || echo 0)"
    echo "config.sha256=$([ -f "$cfg" ] && sha256sum "$cfg" 2>/dev/null | awk '{print $1}')"

    # The preset the user last asked for. Note the file is what the *switcher* reads,
    # so it is authoritative even if the daemon is down.
    if [ -f "$PRESET_FILE" ]; then
        echo "preset.current=$(cat "$PRESET_FILE" 2>/dev/null)"
        echo "preset.present=1"
    else
        echo "preset.current="
        echo "preset.present=0"
    fi

    # A policy reading `userspace` means our takeover is armed right now: that is the
    # only way to tell, and it is also the state that must not be left behind on a
    # crash (see libuperf.sh).
    local armed=0 govs=""
    for d in $(uperf_policy_dirs); do
        local g; g="$(cat $d/scaling_governor 2>/dev/null)"
        govs="$govs$(basename $d)=$g "
        [ "$g" = "userspace" ] && armed=1
    done
    echo "governor.takeover=$armed"
    echo "governor.policies=$govs"
    echo "governor.recorded=$([ -f "$GOVERNOR_STATE" ] && tr '\n' ';' < "$GOVERNOR_STATE" || echo '')"

    # M9: the two status files, plus the watchdog's own pid from the owner lock.
    # `daemon.state=running` with `daemon.count=0` is the signature of a kill (the
    # watchdog reacts to exactly that); `watchdog.state=gave-up` means the restart
    # budget was spent and the platform governor is in charge on purpose.
    local dstate wstate wpid=""
    dstate="$([ -f "$USER_PATH/uperf.state" ] && sed -n 's/^state=//p' "$USER_PATH/uperf.state" 2>/dev/null | head -n 1)"
    wstate="$([ -f "$USER_PATH/uperf_watchdog.state" ] && sed -n 's/^state=//p' "$USER_PATH/uperf_watchdog.state" 2>/dev/null | head -n 1)"
    case "$wstate" in
    "" | stopped) ;;
    *) wpid="$(uperf_watchdog_pid)" ;;
    esac
    echo "daemon.state=$dstate"
    echo "daemon.armed=$([ -f "$USER_PATH/uperf.state" ] && sed -n 's/^armed=//p' "$USER_PATH/uperf.state" 2>/dev/null | head -n 1)"
    echo "watchdog.state=$wstate"
    echo "watchdog.restarts=$([ -f "$USER_PATH/uperf_watchdog.state" ] && sed -n 's/^restarts=//p' "$USER_PATH/uperf_watchdog.state" 2>/dev/null | head -n 1)"
    echo "watchdog.pid=$wpid"
    # The frame source (M8) is a second thing that can be gone: `sf_state=injected` means
    # the mapping is ours, `healthy` that it was already there, `gave-up` that the failure
    # budget is spent. Only meaningful with `UPERF_SF_INJECT=1`; `off` otherwise.
    echo "watchdog.sf_state=$([ -f "$USER_PATH/uperf_watchdog.state" ] && sed -n 's/^sf_state=//p' "$USER_PATH/uperf_watchdog.state" 2>/dev/null | head -n 1)"
    echo "watchdog.sf_injects=$([ -f "$USER_PATH/uperf_watchdog.state" ] && sed -n 's/^sf_injects=//p' "$USER_PATH/uperf_watchdog.state" 2>/dev/null | head -n 1)"
    echo "watchdog.sf_fails=$([ -f "$USER_PATH/uperf_watchdog.state" ] && sed -n 's/^sf_fails=//p' "$USER_PATH/uperf_watchdog.state" 2>/dev/null | head -n 1)"
    # ⑤ the daemon's own frame source (opt-in `UPERF_SF_BINDER=1`): `source=hint` while
    # the injected library's hint is fresh, `source=fps` when the direct-binder leg had
    # to take over (that leg is not even polled while the hint is live).
    local frames="$USER_PATH/uperf_frames.state"
    echo "frame.source=$([ -f "$frames" ] && sed -n 's/^source=//p' "$frames" 2>/dev/null | head -n 1)"
    echo "frame.fps=$([ -f "$frames" ] && sed -n 's/^fps=//p' "$frames" 2>/dev/null | head -n 1)"
    echo "frame.hint_age_ms=$([ -f "$frames" ] && sed -n 's/^hint_age_ms=//p' "$frames" 2>/dev/null | head -n 1)"
    echo "frame.refresh_ns=$([ -f "$frames" ] && sed -n 's/^refresh_ns=//p' "$frames" 2>/dev/null | head -n 1)"
    echo "frame.layer=$([ -f "$frames" ] && sed -n 's/^layer=//p' "$frames" 2>/dev/null | head -n 1)"

    local log="$USER_PATH/uperf_log.txt"
    echo "log.path=$log"
    echo "log.lines=$([ -f "$log" ] && wc -l < "$log" | tr -d ' ' || echo 0)"
    echo "log.bak=$([ -f "$log.bak" ] && echo 1 || echo 0)"

    local boot; boot="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
    echo "uptime.sec=$(awk '{printf "%d", $1}' /proc/uptime 2>/dev/null)"
    echo "boot.id=$boot"
}

print_info() {
    echo "soc.platform=$(getprop ro.board.platform)"
    echo "soc.model=$(getprop ro.soc.model)"
    echo "device.model=$(getprop ro.product.model)"
    echo "device.device=$(getprop ro.product.device)"
    echo "android.release=$(getprop ro.build.version.release)"
    echo "android.sdk=$(getprop ro.build.version.sdk)"
    echo "kernel.release=$(uname -r)"
    echo "selinux=$(getenforce 2>/dev/null)"
    echo "module.dir=$MODULE_PATH"
    echo "module.id=$(grep -m1 '^id=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "module.name=$(grep -m1 '^name=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "module.version=$(grep -m1 '^version=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "module.versioncode=$(grep -m1 '^versionCode=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "module.author=$(grep -m1 '^author=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "module.description=$(grep -m1 '^description=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo "daemon.identity=$(grep -m1 -o 'uperf[^,]*' "$USER_PATH/uperf_log.txt" 2>/dev/null | head -1)"
}

# The write path. The *frontend* decides whether a name is a preset of the loaded
# config (it has the JSON); this only performs the write and reports what the file
# actually contains afterwards, so a silent truncation (see docs/m6b-evidence.md) or
# a read-only filesystem cannot be reported as success.
set_preset() {
    local want="$1"
    if [ -z "$want" ]; then
        echo "preset.set="
        echo "preset.ok=0"
        echo "preset.error=no value given"
        return 1
    fi
    case "$want" in
        *[!a-zA-Z0-9_+-]*)
            echo "preset.set="
            echo "preset.ok=0"
            echo "preset.error=illegal characters"
            return 1
            ;;
    esac
    mkdir -p "$USER_PATH" 2>/dev/null
    printf '%s\n' "$want" > "$PRESET_FILE" 2>/dev/null
    local got; got="$(cat "$PRESET_FILE" 2>/dev/null)"
    echo "preset.want=$want"
    echo "preset.set=$got"
    if [ "$got" = "$want" ]; then
        echo "preset.ok=1"
        [ "$(uperf_pid_count)" -eq 0 ] && echo "preset.note=daemon not running, applies on next start"
    else
        echo "preset.ok=0"
        echo "preset.error=read-back mismatch"
        return 1
    fi
}

print_log() {
    local n="${1:-200}"
    case "$n" in *[!0-9]*) n=200 ;; esac
    [ "$n" -gt 2000 ] && n=2000
    local log="$USER_PATH/uperf_log.txt"
    if [ ! -f "$log" ]; then
        echo "(no log at $log)"
        return 0
    fi
    tail -n "$n" "$log"
}

do_restart() {
    echo "restart.before=$(uperf_pid_count)"
    # The daemon setsid()s itself, so it survives this shell exiting — that is why
    # an in-WebUI restart is safe (a plain background child would be taken down with
    # the exec's process group).
    uperf_stop
    uperf_start
    local n; n="$(uperf_pid_count)"
    echo "restart.after=$n"
    [ "$n" -gt 0 ] && echo "restart.ok=1" || echo "restart.ok=0"
}

case "$1" in
    status) print_status ;;
    info) print_info ;;
    all)
        print_info
        print_status
        ;;
    set-preset) set_preset "$2" ;;
    log) print_log "$2" ;;
    restart) do_restart ;;
    *)
        echo "Usage: webui.sh {status|info|all|set-preset <name>|log [n]|restart}" >&2
        exit 2
        ;;
esac
