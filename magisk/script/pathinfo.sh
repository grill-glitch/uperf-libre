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

MODULE_PATH="$(dirname $(readlink -f "$0"))"
MODULE_PATH="${MODULE_PATH%\/script}"
SCRIPT_PATH="$MODULE_PATH/script"
BIN_PATH="$MODULE_PATH/bin"
FLAG_PATH="$MODULE_PATH/flag"

USER_PATH="/sdcard/Android/yc/uperf"
LOG_FILE="$USER_PATH/initsvc.log"

# No private toolbox on PATH any more: the module used to prepend its bundled busybox
# directory here, which shadowed the ROM's own applets (and once made `ps` lie about a
# running daemon, docs/records/m7-evidence.md §7.3). Every command the scripts use is now
# expected from the ROM; docs/records/shell-dependencies.md is the list, together with
# what a too-old toybox would be missing and how that degrades.

