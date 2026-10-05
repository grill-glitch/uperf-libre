#!/bin/bash
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
# Build entry point, modelled on the vendored dfps build.sh (same NDK prefixes, same
# cmake invocation pattern). Added tasks: check, push, run, test.
#
# usage: sh build.sh [Release|Debug] <task> [task...]
#   tasks: make pack push run install reboot clean check all
# env:
#   ANDROID_NDK   default ~/Android/Sdk/ndk/android-ndk-r30
#   ADB_SERIAL    select device for push/run/install/reboot
#   TOOL          adb binary, default ~/Android/Sdk/platform-tools/adb
#   RUN_CFG       config file used by `run` (default: a stub on the device)

set -e

BASEDIR="$(dirname $(readlink -f "$0"))"
BUILD_TYPE="${1:-Release}"
BUILD_TASKS="${*:2}"
BUILD_TASKS="${BUILD_TASKS:-all}"

ANDROID_NDK="${ANDROID_NDK:-$HOME/Android/Sdk/ndk/android-ndk-r30}"
TOOLCHAIN_PREBUILT="$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64"
TOOLCHAIN_BIN="$TOOLCHAIN_PREBUILT/bin"
ARM64_PREFIX=aarch64-linux-android23
ARM64_TARGET=aarch64-linux-android

TOOL="${TOOL:-$HOME/Android/Sdk/platform-tools/adb}"
ADB="$TOOL ${ADB_SERIAL:+-s $ADB_SERIAL}"
REMOTE_DIR=/data/local/tmp
RUN_CFG="${RUN_CFG:-$REMOTE_DIR/uperf_m0_stub.json}"
RUN_LOG="$REMOTE_DIR/uperf_m0_log.txt"

BUILD_DIR="$BASEDIR/build"
STAGE_DIR="$BUILD_DIR/stage"
PKG_DIR="$BUILD_DIR/package"
BINARY="$BUILD_DIR/$ARM64_PREFIX/runnable/uperf"

# $1:prefix $2:targets
build_targets() {
    mkdir -p $BUILD_DIR/$1
    cmake \
        -DCMAKE_BUILD_TYPE=$BUILD_TYPE \
        -DCMAKE_C_COMPILER="$TOOLCHAIN_BIN/$1-clang" \
        -DCMAKE_CXX_COMPILER="$TOOLCHAIN_BIN/$1-clang++" \
        -H$BASEDIR \
        -B$BUILD_DIR/$1 \
        -G "Unix Makefiles"
    cmake --build $BUILD_DIR/$1 --config $BUILD_TYPE --target $2 -j
}

# Rust staticlib for the target ABI. Must run before cmake links: a stale
# libuperf_core.a silently produces an ABI-mismatched binary (this bit us once —
# the bridge's write_log gained a `tag` parameter and the C++ side read a length
# as a pointer).
build_rust() {
    echo ">>> Making libuperf_core.a (Rust, $ARM64_TARGET)"
    (cd $BASEDIR/rust && cargo build -p uperf-core --release --target $ARM64_TARGET)
}

make_uperf() {
    echo ">>> Making uperf ($BUILD_TYPE, $ARM64_PREFIX)"
    build_rust
    build_targets $ARM64_PREFIX uperf
}

# M0 artifact assertions (AGENT.md §9.3). Prints every measured value, fails on a
# violated invariant.
check_uperf() {
    local bin="$BINARY"
    echo ">>> Checking $bin"
    [ -f "$bin" ] || { echo " !! binary not built"; exit 1; }

    local elf="$TOOLCHAIN_BIN/llvm-readelf"
    local sz=$(stat -c %s "$bin")
    # LC_ALL=C + strip brackets: llvm-readelf prints NEEDED as "[libm.so]" and localises
    # the line in non-C locales.
    local needed=$(LC_ALL=C $elf -dW "$bin" | grep NEEDED | sed 's/[][]//g' | awk '{print $NF}' | sort | tr '\n' ' ')
    local interp=$(LC_ALL=C $elf -lW "$bin" | grep -c "INTERP")
    local symtab=$(LC_ALL=C $elf -SW "$bin" | grep -c "\.symtab" || true)
    local relro=$(LC_ALL=C $elf -lW "$bin" | grep -oE "GNU_RELRO" | head -1)
    local bindnow=$(LC_ALL=C $elf -dW "$bin" | grep -c "BIND_NOW" || true)

    echo "    size      : $sz bytes ($(echo "scale=2; $sz/1024/1024" | bc) MiB)"
    echo "    NEEDED    : $needed"
    echo "    .interp   : $interp"
    echo "    .symtab   : $symtab"
    echo "    RELRO     : ${relro:-none}  BIND_NOW: $bindnow"

    case "$bin" in
    /home/*) file "$bin" | sed 's/^/    file: /' ;;
    esac

    [ "$interp" -eq 1 ] || { echo " !! not a dynamic PIE"; exit 1; }
    [ "$symtab" -le 1 ] || { echo " !! not stripped"; exit 1; }
    # the original dev-22.09.04 binary links exactly libm/libdl/libc
    local unexpected=$(echo "$needed" | tr ' ' '\n' | grep -vE "^(libm\.so|libdl\.so|libc\.so|)$" | tr '\n' ' ')
    [ -z "$unexpected" ] || { echo " !! unexpected NEEDED libs: $unexpected"; exit 1; }
    [ "$sz" -lt 3145728 ] || { echo " !! binary too large (> 3 MiB)"; exit 1; }
    echo "    -> OK"
}

pack_uperf() {
    echo ">>> Packing uperf-magisk.zip (staged copy; the committed magisk/bin/uperf is left untouched)"
    rm -rf $STAGE_DIR
    mkdir -p $STAGE_DIR $PKG_DIR
    cp -a $BASEDIR/magisk/. $STAGE_DIR/
    cp -f $BINARY $STAGE_DIR/bin/uperf
    cp -f $BASEDIR/LICENSE $BASEDIR/NOTICE $STAGE_DIR/
    (cd $STAGE_DIR && zip -q -9 -r $PKG_DIR/uperf-magisk.zip .)
    echo "    -> $PKG_DIR/uperf-magisk.zip ($(stat -c %s $PKG_DIR/uperf-magisk.zip) bytes)"
}

push_files() {
    echo ">>> Pushing binary to $REMOTE_DIR"
    $ADB push "$BINARY" $REMOTE_DIR/uperf
    # M0 does not parse the config yet, it only requires it to exist (access R_OK)
    echo '{"_m0_stub": "config parsing lands in M2"}' > $BUILD_DIR/uperf_m0_stub.json
    $ADB push $BUILD_DIR/uperf_m0_stub.json $REMOTE_DIR/uperf_m0_stub.json
    $ADB shell chmod 755 $REMOTE_DIR/uperf
}

# foreground run with the M0 stub config, log streamed back
run_uperf() {
    echo ">>> Running uperf on device (config=$RUN_CFG log=$RUN_LOG)"
    $ADB shell su -c "rm -f $RUN_LOG; $REMOTE_DIR/uperf $RUN_CFG -o $RUN_LOG"
    sleep 3
    echo ">>> ps"
    $ADB shell su -c "ps -A -o PID,PPID,NAME | grep -E 'uperf' || true"
    echo ">>> $RUN_LOG"
    $ADB shell su -c "cat $RUN_LOG"
    echo ">>> killing"
    $ADB shell su -c "pkill -f '$REMOTE_DIR/uperf' || true"
}

install_module() {
    echo ">>> Pushing and installing uperf-magisk.zip"
    $ADB push $PKG_DIR/uperf-magisk.zip $REMOTE_DIR
    $ADB shell su -c "magisk --install-module $REMOTE_DIR/uperf-magisk.zip"
}

reboot_device() {
    echo ">>> Rebooting device"
    $ADB shell reboot
}

clean() {
    echo ">>> Cleaning"
    rm -rf $BUILD_DIR
}

do_task() {
    case $1 in
    make) make_uperf ;;
    check) check_uperf ;;
    pack) pack_uperf ;;
    push) push_files ;;
    run) run_uperf ;;
    install) install_module ;;
    reboot) reboot_device ;;
    clean) clean ;;
    all)
        make_uperf
        check_uperf
        pack_uperf
        ;;
    *)
        echo " ! Unknown task name $1"
        exit 1
        ;;
    esac
}

for t in $BUILD_TASKS; do
    do_task $t
done
