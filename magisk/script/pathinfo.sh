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

# Quote the inner substitution: unquoted, `readlink -f` output is word-split and
# glob-expanded before `dirname` sees it, so a module path containing a space (or a glob
# character) produced a truncated root - and every derived path with it.
MODULE_PATH="$(dirname "$(readlink -f "$0")")"
MODULE_PATH="${MODULE_PATH%\/script}"
# Resolved from `$0`, which is only the module's script when this file is *executed* (or
# sourced from one). Sourced from an interactive shell, from `sh -c`, or from the WebUI's
# `exec`, `$0` is `/system/bin/sh` and the guess above is meaningless. Aborting there was
# worse than the bug it guarded: `return 1` left the caller with an empty `USER_PATH` and it
# then wrote `/uperf_log.txt` at the filesystem root. Discover the installed module instead,
# and only warn (never abort) when even that fails.
if [ ! -f "$MODULE_PATH/module.prop" ]; then
    for _cand in /data/adb/modules/*/; do
        if [ -f "${_cand}module.prop" ] && [ -d "${_cand}script" ] &&
            [ -f "${_cand}script/pathinfo.sh" ]; then
            MODULE_PATH="${_cand%/}"
            break
        fi
    done
fi
if [ ! -f "$MODULE_PATH/module.prop" ]; then
    echo "pathinfo: cannot resolve the module root from \$0='$0' (got '$MODULE_PATH'); using the default user path" >&2
    _unresolved=1
fi
SCRIPT_PATH="$MODULE_PATH/script"
BIN_PATH="$MODULE_PATH/bin"
FLAG_PATH="$MODULE_PATH/flag"

# The daemon's own default is the same directory, so an unresolved module root still writes
# where the module reads.
USER_PATH="/sdcard/Android/yc/uperf"
unset _unresolved
# Absolute CPU root: `$CPU/cpuN/...` was used by platform_special.sh and powercfg_once.sh
# but never defined, so those writes targeted "/cpuN/..." and silently did nothing.
CPU="/sys/devices/system/cpu"
LOG_FILE="$USER_PATH/initsvc.log"
# Panel state file read by `read_cfg_value`/`write_cfg_value` (libcommon.sh). It was never
# defined anywhere, so every read returned "" and every write failed with "can't create".
PANEL_FILE="$USER_PATH/panel_cfg.txt"

# No private toolbox on PATH any more: the module used to prepend its bundled busybox
# directory here, which shadowed the ROM's own applets (and once made `ps` lie about a
# running daemon, docs/records/m7-evidence.md §7.3). Every command the scripts use is now
# expected from the ROM; docs/records/shell-dependencies.md is the list, together with
# what a too-old toybox would be missing and how that degrades.

