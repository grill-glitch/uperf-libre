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
# Basic tool functions
###############################

# $1:value $2:filepaths
lock_val() {
    for p in $2; do
        if [ -f "$p" ]; then
            # `chown`/`chmod` are best-effort. On `/proc/sys` nodes the ksu domain cannot
            # change ownership (EACCES) while the write itself works, and on
            # `qcom-cpufreq-hw` the driver rejects the write regardless of the mode — so
            # the noise read as a failure when it was neither. What matters is the value,
            # and a failed write now says so once. The 0444 at the end still marks the
            # node as ours.
            chown root:root "$p" 2>/dev/null
            chmod 0666 "$p" 2>/dev/null
            if ! echo "$1" >"$p" 2>/dev/null; then
                echo "uperf: '$p' rejected $1 (kernel-locked; leaving the current value)"
            fi
            chmod 0444 "$p" 2>/dev/null
        fi
    done
}

# $1:value $2:filepaths
mask_val() {
    touch /data/local/tmp/mount_mask
    for p in $2; do
        if [ -f "$p" ]; then
            # Every step checked: on failure the bind mount used to succeed with the *old*
            # value still in the node, or silently leave it writable.
            umount "$p" 2>/dev/null
            chmod 0666 "$p" 2>/dev/null
            if ! echo "$1" >"$p" 2>/dev/null; then
                echo "uperf: '$p' rejected $1"
                continue
            fi
            mount --bind /data/local/tmp/mount_mask "$p" 2>/dev/null ||
                echo "uperf: '$p' not masked (bind mount failed)"
        fi
    done
}

# $1:value $2:filepaths
mutate() {
    for p in $2; do
        if [ -f "$p" ]; then
            chmod 0666 "$p"
            echo "$1" >"$p"
        fi
    done
}

# $1:file path
lock() {
    if [ -f "$1" ]; then
        # `$p`, not `$1`: `p` is the leaked loop variable of the last lock_val/mutate call,
        # so this chowned an unrelated node.
        chown root:root "$1"
        chmod 0444 "$1"
    fi
}

# $1:value $2:list
has_val_in_list() {
    for item in $2; do
        if [ "$1" = "$item" ]; then
            echo "true"
            return
        fi
    done
    echo "false"
}

###############################
# Config File Operator
###############################

# $1:key $return:value(string)
read_cfg_value() {
    local value=""
    if [ -f "$PANEL_FILE" ]; then
        # `tr -d ' '` mangled multi-token values ("0 3200000" -> "0320000000") and `cut -f2`
        # truncated at a second '='; the key is also a literal, not a regex.
        value="$(grep -F -m 1 "$1=" "$PANEL_FILE" 2>/dev/null | sed "s/^$1=//")"
    fi
    echo "$value"
}

# $1:content
write_panel() {
    [ -n "$PANEL_FILE" ] || return 1
    echo "$1" >>"$PANEL_FILE"
}

clear_panel() {
    [ -n "$PANEL_FILE" ] || return 1
    true >"$PANEL_FILE"
}

wait_until_login() {
    # Bounded: an unbounded loop meant a device whose boot never completes (or a ROM without
    # `getprop`) hung the whole boot script here, forever.
    local tries=0
    while [ "$(getprop sys.boot_completed 2>/dev/null)" != "1" ]; do
        sleep 1
        tries=$((tries + 1))
        [ "$tries" -ge 300 ] && return 1
    done

    # We cannot rw "/sdcard" before the user unlocks the screen. Verify the *write* and delete
    # any stale file first: a leftover from a killed previous run made the loop exit at once.
    local test_file="/sdcard/Android/.PERMISSION_TEST"
    local retry=0
    rm -f "$test_file" 2>/dev/null
    while ! (true >"$test_file" 2>/dev/null && [ -f "$test_file" ]); do
        [ "$retry" -ge 300 ] && return 1
        retry=$((retry + 1))
        sleep 1
    done
    rm "$test_file"
}

###############################
# key=value state files
###############################

# Read one key from a `key=value` file (uperf.state / uperf_watchdog.state /
# uperf_frames.state), or print nothing.
#
# One reader, because three scripts used to spell this out (`sed -n 's/^key=//p'`),
# and the daemon writes the format from exactly one place (`status.rs` /
# `frame_state::Published::render`). A field rename therefore has one shell site to
# update, not twelve.
#
# $1:file  $2:key
state_get() {
    [ -f "$1" ] || return 0
    sed -n "s/^$2=//p" "$1" 2>/dev/null | head -n 1
}

# $1:content
log() {
    echo "$1" >>"$LOG_FILE"
}

clear_log() {
    true >"$LOG_FILE"
}
