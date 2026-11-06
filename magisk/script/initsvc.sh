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
    echo "uperf: missing shell command(s):$missing -- ROM toybox too old? tuning paths using them are no-ops" >>"$LOG_FILE"
}
uperf_check_shell_deps

# support vtools
cp -af $SCRIPT_PATH/vtools_powercfg.sh /data/powercfg.sh
cp -af $SCRIPT_PATH/vtools_powercfg.sh /data/powercfg-base.sh
cp -af $SCRIPT_PATH/powercfg.json /data/powercfg.json
chmod 755 /data/powercfg.sh
chmod 755 /data/powercfg-base.sh
echo "sh $SCRIPT_PATH/powercfg_main.sh \"\$1\"" >>/data/powercfg.sh

wait_until_login

sh $SCRIPT_PATH/powercfg_once.sh
sh $SCRIPT_PATH/platform_special.sh
sh $SCRIPT_PATH/miui_migt.sh
uperf_start
