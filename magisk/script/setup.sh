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
# Install-time entry: seeds the matched SoC config into the user directory and
# fixes permissions. Called from customize.sh (KernelSU/Magisk install).
#
# Output rules for this file:
#   * English only - no Chinese, no emoji. The install log has to stay readable in
#     a terminal that may not have the fonts, and it is the first thing a user sees.
#   * Always print attribution, license, and the no-warranty notice.

BASEDIR="$(dirname $(readlink -f "$0"))"
. $BASEDIR/pathinfo.sh
. $BASEDIR/libsysinfo.sh

# $1:error_message
abort() {
    echo "$1"
    echo "! uperf-libre installation failed."
    exit 1
}

# $1:file_node $2:owner $3:group $4:permission $5:secontext
set_perm() {
    chown $2:$3 $1
    chmod $4 $1
    chcon $5 $1
}

# $1:directory $2:owner $3:group $4:dir_permission $5:file_permission $6:secontext
set_perm_recursive() {
    find $1 -type d 2>/dev/null | while read dir; do
        set_perm $dir $2 $3 $4 $6
    done
    find $1 -type f -o -type l 2>/dev/null | while read file; do
        set_perm $file $2 $3 $5 $6
    done
}

print_banner() {
    local version
    version="$(grep -m1 '^version=' "$MODULE_PATH/module.prop" 2>/dev/null | cut -d= -f2-)"
    echo ""
    echo "* uperf-libre - userspace performance controller, policy engine in Rust"
    echo "* version     : ${version:-unknown}"
    echo "* author      : grill-glitch (rewrite)"
    echo "* upstream    : Matt Yang (yc9559), yinwanxi (Uperf-Game-Turbo)"
    echo "* source      : https://github.com/grill-glitch/uperf-libre"
    echo "* license     : Apache-2.0 - see LICENSE and NOTICE in this module"
    echo ""
    echo "* NO WARRANTY. Installed and used entirely at your own risk."
    echo "* This module writes kernel and system tunables. Make sure you can boot"
    echo "* without it (Magisk/KernelSU safe mode) before you install."
    echo ""
    echo "* before you continue:"
    echo "*   - do not break the running environment: do not change the CPU governor"
    echo "*     or the cpuset layout by hand while the daemon runs"
    echo "*   - conflicts with other frequency-limiting or optimizer modules"
    echo "*   - may conflict with third-party kernels; ask the kernel author first"
    echo "*   - fast mode wants cooling; removing thermal limits is your own choice"
    echo ""
}

install_uperf() {
    echo "- Finding platform specified config"
    echo "- ro.board.platform=$(getprop ro.board.platform)"
    echo "- ro.product.board=$(getprop ro.product.board)"

    local target
    local cfgname
    target="$(getprop ro.board.platform)"
    cfgname="$(get_config_name $target)"
    if [ "$cfgname" == "unsupported" ]; then
        target="$(getprop ro.product.board)"
        cfgname="$(get_config_name $target)"
    fi

    if [ "$cfgname" == "unsupported" ] || [ ! -f $MODULE_PATH/config/$cfgname.json ]; then
        abort "! Target [$target] not supported."
    fi

    echo "- Uperf config is located at $USER_PATH"
    mkdir -p $USER_PATH
    mv -f $USER_PATH/uperf.json $USER_PATH/uperf.json.bak
    cp -f $MODULE_PATH/config/$cfgname.json $USER_PATH/uperf.json
    # Kept for libuperf.sh::uperf_ensure_config(): `config/` is deleted below, so this
    # is the only copy left on the device, and it is what makes a lost user directory
    # recoverable instead of requiring a reinstall.
    cp -f $MODULE_PATH/config/$cfgname.json $USER_PATH/uperf.json.default
    [ ! -e "$USER_PATH/perapp_powermode.txt" ] && cp $MODULE_PATH/config/perapp_powermode.txt $USER_PATH/perapp_powermode.txt
    rm -rf $MODULE_PATH/config
    set_perm_recursive $BIN_PATH 0 0 0755 0755 u:object_r:system_file:s0
}

fix_module_prop() {
    mkdir -p /data/adb/modules/uperf/
    cp -f "$MODULE_PATH/module.prop" /data/adb/modules/uperf/module.prop
}

# Kept from upstream, currently unused (the call is commented out below): creates the
# empty vendor perf config files so the ROM stops reading its own perf tuning.
unlock_limit() {
    if [ ! -d $MODPATH/system/vendor/etc/perf/ ]; then
        dir=$MODPATH/system/vendor/etc/perf/
        mkdir -p $dir
    fi
    for i in `ls /system/vendor/etc/perf/`; do
        touch $dir/$i
    done
}

print_banner

# ⑩: refuse an unsupported ABI before touching anything. Non-zero here aborts the
# install (customize.sh: `[ "$?" != "0" ] && abort`).
require_aarch64 || abort "! uperf-libre: this device is $(module_abi); see the reason above."

install_uperf
# unlock_limit
fix_module_prop

echo "* uperf-libre installed successfully."
echo "* reboot to activate."
echo "* author grill-glitch - Apache-2.0 - no warranty, use at your own risk."
