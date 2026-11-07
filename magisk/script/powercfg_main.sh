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

BASEDIR="$(dirname "$(readlink -f "$0")")"
# A failed source is non-fatal in mksh: execution continued with USER_PATH unset, so the
# write below landed on /cur_powermode.txt. Validate instead.
PATHINFO_FILE="$BASEDIR/pathinfo.sh"
[ -r "$PATHINFO_FILE" ] || { echo "Failed to source $PATHINFO_FILE." >&2; exit 1; }
. "$PATHINFO_FILE"
[ -n "$USER_PATH" ] || { echo "USER_PATH is not set by pathinfo.sh." >&2; exit 1; }

action="$1"
case "$action" in
"powersave" | "balance" | "performance" | "fast" | "auto") ;;
"pedestal") action="performance" ;;
"init") action="balance" ;;
*) echo "Failed to apply unknown action '$1'." >&2; exit 1 ;;
esac

mkdir -p "$USER_PATH" 2>/dev/null
if ! echo "$action" >"$USER_PATH/cur_powermode.txt" 2>/dev/null; then
    echo "Failed to write $USER_PATH/cur_powermode.txt" >&2
    exit 1
fi
