#!/vendor/bin/sh
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

BASEDIR="$(dirname $(readlink -f "$0"))"
. $BASEDIR/pathinfo.sh
. $BASEDIR/libcommon.sh
. $BASEDIR/libuperf.sh

# The module no longer ships a toolbox. Everything the scripts call must come from the
# ROM (docs/records/shell-dependencies.md). A missing command is a silent no-op in the
# middle of a tuning pass, so name it once at boot instead of letting it be noticed.
uperf_check_shell_deps() {
    local missing="" c
    for c in awk sed grep cut tr sort head tail wc seq sleep usleep date find \
             killall ps renice chrt taskset chattr chown readlink which sha256sum \
             uname touch mkdir mv cp rm ls basename dirname cat chmod id kill; do
        command -v "$c" >/dev/null 2>&1 || missing="$missing $c"
    done
    [ -z "$missing" ] && return 0
    # /sdcard is not mounted yet at this point (the mkdir happens inside uperf_start), so
    # the redirect used to fail and the warning was lost. Create the directory first and
    # fall back to the kernel log.
    mkdir -p "$USER_PATH" 2>/dev/null
    if ! echo "uperf: missing shell command(s):$missing -- ROM toybox too old? tuning paths using them are no-ops" >>"$LOG_FILE" 2>/dev/null; then
        echo "uperf: missing shell command(s):$missing" >/dev/kmsg 2>/dev/null
    fi
}
uperf_check_shell_deps

# support vtools
# Unchecked copies left a stale /data/powercfg.sh that then got the appended line again on
# the next boot, running powercfg_main.sh several times per event.
cp -af "$SCRIPT_PATH/vtools_powercfg.sh" /data/powercfg.sh || exit 1
cp -af "$SCRIPT_PATH/vtools_powercfg.sh" /data/powercfg-base.sh || exit 1
cp -af "$SCRIPT_PATH/powercfg.json" /data/powercfg.json || exit 1
chmod 755 /data/powercfg.sh || exit 1
chmod 755 /data/powercfg-base.sh || exit 1
# The appended line is the only thing that makes the wrapper work: verify the target exists
# (and the append landed) instead of shipping a wrapper that exits 0 and tunes nothing.
if [ ! -f "$SCRIPT_PATH/powercfg_main.sh" ]; then
    echo "uperf: missing $SCRIPT_PATH/powercfg_main.sh, vtools powercfg will be inert" >/dev/kmsg 2>/dev/null
fi
# Quoted: the emitted command is executed later by vtools' shell.
echo "sh \"$SCRIPT_PATH/powercfg_main.sh\" \"\$1\"" >>/data/powercfg.sh || exit 1
grep -q "powercfg_main.sh" /data/powercfg.sh || echo "uperf: powercfg wrapper has no command appended" >/dev/kmsg 2>/dev/null

wait_until_login || echo "uperf: login not detected in time; continuing" >/dev/kmsg 2>/dev/null

# Report each step: a missing script or a failed pass used to leave the device on a
# half-applied boot configuration with no trace in the log.
for _s in powercfg_once.sh platform_special.sh miui_migt.sh; do
    if [ ! -f "$SCRIPT_PATH/$_s" ]; then
        echo "uperf: missing $SCRIPT_PATH/$_s" >/dev/kmsg 2>/dev/null
        continue
    fi
    sh "$SCRIPT_PATH/$_s" || echo "uperf: $_s failed" >/dev/kmsg 2>/dev/null
done
uperf_start || echo "uperf: uperf_start failed" >/dev/kmsg 2>/dev/null
