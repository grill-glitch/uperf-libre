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

# MR author: railjty
USER_PATH=/sdcard/Android/yc/uperf

wait_until_login() {
    # in case of /data encryption is disabled
    while [ "$(getprop sys.boot_completed)" != "1" ]; do
        sleep 1
    done

    # we doesn't have the permission to rw "/sdcard" before the user unlocks the screen
    local test_file="/sdcard/Android/.PERMISSION_TEST"
    true >"$test_file"
    while [ ! -f "$test_file" ]; do
        true >"$test_file"
        sleep 1
    done
    rm "$test_file"
}

on_remove() {
    wait_until_login

    # Stop the daemon and undo the userspace-governor takeover before the module
    # (and its binary) disappears. Without this a running daemon keeps the
    # policies pinned, and after the module is gone nothing can restore them.
    . $MODDIR/script/libuperf.sh 2>/dev/null || . $MODDIR/libuperf.sh 2>/dev/null
    uperf_stop 2>/dev/null || killall uperf 2>/dev/null
    uperf_restore_governors 2>/dev/null

    # keep user perapp config
    cp -af $USER_PATH/perapp_powermode.txt /sdcard/
    rm -rf $USER_PATH
    mkdir -p $USER_PATH
    mv /sdcard/perapp_powermode.txt $USER_PATH/

    rm -f /data/powercfg*
}

# do not block boot
(on_remove &)
