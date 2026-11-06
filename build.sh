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
#   the build type is optional; `sh build.sh pack` means `sh build.sh Release pack`
#   tasks: make pack push run install reboot clean check all
# env:
#   ANDROID_NDK   default ~/Android/Sdk/ndk/android-ndk-r30
#   ADB_SERIAL    select device for push/run/install/reboot
#   TOOL          adb binary, default ~/Android/Sdk/platform-tools/adb
#   RUN_CFG       config file used by `run` (default: a stub on the device)

set -e

BASEDIR="$(dirname $(readlink -f "$0"))"

# The build type is optional, but a *task* must never be read as one. `sh build.sh pack`
# (the form the README advertised) used to become BUILD_TYPE=pack with the task list
# defaulting to `all`: cmake got -DCMAKE_BUILD_TYPE=pack, matched neither branch in
# cpp/uperf/CMakeLists.txt, and linked the driver default — libc++_shared, no -flto, no
# strip. The result was a 7.4 MiB binary that `check` rejects (§ unexpected NEEDED libs)
# and that cannot start on a device without /system/lib64/libc++_shared.so — and the
# failure only surfaced in `check`, *after* `pack` had already written a zip. See issue #1.
usage_uperf() {
    echo "usage: sh build.sh [Release|Debug] <task> [task...]"
    echo "  tasks: make pack push run install reboot clean check all  (default: all)"
    echo "  the build type may be omitted: 'sh build.sh pack' == 'sh build.sh Release pack'"
}

BUILD_TYPE=Release
case "${1:-}" in
Release | Debug)
    BUILD_TYPE="$1"
    shift
    ;;
"") ;;
make | pack | push | run | install | reboot | clean | check | all) ;;
*)
    echo " !! neither a build type (Release|Debug) nor a task: $1"
    usage_uperf
    exit 1
    ;;
esac
BUILD_TASKS="${*:-all}"

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

# M8: SfAnalysis injection library (cdylib). Replaces the vendor's
# closed-source `libsfanalysis.so` with our `libsfanalysis_rs.so`.
# Documented in docs/spec/sfanalysis.md and AGENT.md §12.1.
build_sfanalysis() {
    echo ">>> Making libsfanalysis_rs.so (Rust cdylib, $ARM64_TARGET, profile sfanalysis)"
    # NOT --release: this library is injected into surfaceflinger, so it is built with
    # the `sfanalysis` profile (release settings, but `panic = "unwind"` — with
    # `panic = "abort"` a panic inside the library would abort SurfaceFlinger).
    # See the workspace Cargo.toml.
    (cd $BASEDIR/rust && cargo build -p uperf-sfanalysis --profile sfanalysis --target $ARM64_TARGET)
    local so="$BASEDIR/rust/target/$ARM64_TARGET/sfanalysis/libsfanalysis_rs.so"
    [ -f "$so" ] || { echo " !! libsfanalysis_rs.so was not produced"; exit 1; }
    echo "    -> $so ($(stat -c %s "$so") bytes)"
}

make_uperf() {
    echo ">>> Making uperf ($BUILD_TYPE, $ARM64_PREFIX)"
    build_rust
    build_sfanalysis
    build_targets $ARM64_PREFIX uperf
}

# The WebUI is built into magisk/webroot (KernelSU serves `<module>/webroot`), so it
# travels with the module like any other file. The build output is deliberately not
# committed — same reason the binary is not: what ships must be what was just built.
build_webui() {
    local dir="$BASEDIR/webui"
    [ -d "$dir" ] || { echo " !! webui/ is missing"; exit 1; }
    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
        echo " !! node and npm are required to build the WebUI"
        echo "    install node, then re-run; or build webui/ elsewhere and copy the"
        echo "    result to magisk/webroot/ by hand"
        exit 1
    fi
    echo ">>> Building WebUI"
    # `npm ci` when the lockfile exists (reproducible), otherwise a plain install.
    (cd "$dir" && { [ -f package-lock.json ] && npm ci --no-audit --no-fund >/dev/null 2>&1 || npm install --no-audit --no-fund >/dev/null 2>&1; })
    (cd "$dir" && npm run build) || { echo " !! webui build failed"; exit 1; }
    local idx="$BASEDIR/magisk/webroot/index.html"
    [ -f "$idx" ] || { echo " !! magisk/webroot/index.html was not produced"; exit 1; }
    echo "    -> magisk/webroot ($(du -sh "$BASEDIR/magisk/webroot" | awk '{print $1}'))"
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

    # The module tree must contain THIS build. That single assertion covers both
    # failure modes worth guarding: the closed-source upstream binary still sitting
    # in magisk/bin/uperf (which is what this project exists to replace), and a
    # stale copy left behind by an earlier build. Upstream's dev-22.09.04 is
    # 1461512 bytes; this build is not.
    # The module binary is a **build product**, not tracked source (2.1 MB of binary in
    # git is what `git filter-repo` had to purge from this repository's history). What
    # must hold: whatever copy exists — the staged module tree that gets zipped, or a
    # local `magisk/bin/uperf` left by `pack` — equals *this* build, and no copy of the
    # vendor binary comes back. A missing tree copy is normal in a fresh clone.
    local module_bin="$BASEDIR/magisk/bin/uperf"
    local staged_bin="$STAGE_DIR/bin/uperf"
    local checked=0
    for candidate in "$module_bin" "$staged_bin"; do
        [ -f "$candidate" ] || continue
        checked=$((checked + 1))
        if ! cmp -s "$candidate" "$bin"; then
            echo " !! ${candidate#$BASEDIR/} differs from the built binary"
            echo "    module: $(stat -c %s "$candidate") bytes"
            echo "    build : $sz bytes"
            echo "    fix   : sh build.sh $BUILD_TYPE pack"
            exit 1
        fi
    done
    if [ "$checked" -gt 0 ]; then
        echo "    module bin: $checked copy/copies match this build (staged: $([ -f "$staged_bin" ] && echo yes || echo no))"
    else
        echo "    module bin: none staged yet (run 'sh build.sh $BUILD_TYPE pack' to product the module zip)"
    fi
    if [ -f "$module_bin" ] && [ "$(stat -c %s "$module_bin")" = "1461512" ]; then
        echo " !! magisk/bin/uperf has the size of the upstream close-source binary"
        exit 1
    fi
    [ "$sz" -lt 3145728 ] || { echo " !! binary too large (> 3 MiB)"; exit 1; }

    # The WebUI must be a real build of webui/, not a leftover or a placeholder. The
    # bundle is grepped for the two things that only a resolved build has: the control
    # script path it calls, and the absence of the bare import specifier (which proves
    # `kernelsu-alt` was bundled rather than left as a browser-unresolvable import).
    local webroot="$BASEDIR/magisk/webroot"
    local bundle
    bundle=$(ls "$webroot"/assets/*.js 2>/dev/null | head -1)
    if [ ! -f "$webroot/index.html" ] || [ -z "$bundle" ]; then
        echo " !! magisk/webroot is missing or empty; run 'sh build.sh $BUILD_TYPE pack'"
        exit 1
    fi
    if ! grep -q "script/webui.sh" "$bundle"; then
        echo " !! the WebUI bundle does not reference the control script"
        exit 1
    fi
    if grep -qE 'from *"kernelsu-alt"|require\("kernelsu-alt"\)' "$bundle"; then
        echo " !! the WebUI bundle still imports 'kernelsu-alt' unbundled"
        exit 1
    fi
    echo "    webui     : index.html + $(basename "$bundle") ($(stat -c %s "$bundle") bytes), imports resolved"

    # M8: 0 closed-source blobs in the published module tree (AGENT.md §1).
    # The shipped `libsfanalysis.so` from yc9559/uperf is replaced by
    # `libsfanalysis_rs.so`. Assert both:
    #   - libsfanalysis_rs.so exists and matches this build
    #   - no vendor libsfanalysis.so sneaks back into magisk/bin
    local vendor_so="$BASEDIR/magisk/bin/libsfanalysis.so"
    if [ -f "$vendor_so" ]; then
        echo " !! vendor libsfanalysis.so is still in magisk/bin (M8 0-blob invariant violated)"
        exit 1
    fi
    local rs_so="$BASEDIR/magisk/bin/libsfanalysis_rs.so"
    local rs_build="$BASEDIR/rust/target/$ARM64_TARGET/sfanalysis/libsfanalysis_rs.so"
    if [ ! -f "$rs_build" ]; then
        echo " !! libsfanalysis_rs.so was not built (run 'sh build.sh $BUILD_TYPE make')"
        exit 1
    fi
    if [ -f "$rs_so" ] && ! cmp -s "$rs_so" "$rs_build"; then
        echo " !! magisk/bin/libsfanalysis_rs.so differs from the built artifact"
        exit 1
    fi
    # Sanity: the .so must be a stripped aarch64 ELF with our xh_refresh_loop
    # hint byte protocol — verify the .rodata contains the path the consumer
    # in uperf-core reads from.
    if ! LC_ALL=C strings "$rs_build" | grep -q "xh_refresh_loop"; then
        echo " !! libsfanalysis_rs.so does not advertise hooking xh_refresh_loop"
        exit 1
    fi
    echo "    sfanalysis: libsfanalysis_rs.so matches build, vendor libsfanalysis.so absent"

    # The module tree has exactly one copy of every library.
    #
    # `magisk/` used to carry a stale second copy of the whole script tree at its root
    # (initsvc/libcommon/libuperf/libcgroup/libsysinfo/libpowercfg/pathinfo/powercfg_*/
    # miui_migt/platform_special/vtools_powercfg — 1662 lines). `service.sh` runs
    # `script/initsvc.sh`, so the root copies were dead, but they were self-consistent
    # (the root initsvc.sh sourced the root libuperf.sh), and four of them had already
    # drifted from the live ones — including `libuperf.sh`, i.e. the governor
    # stop/restore path. `cp -a magisk/.` shipped both, so editing the wrong copy was a
    # silent no-op on the device.
    #
    # The root stays the entry points Magisk/KernelSU look for by name; everything else
    # lives in `script/`.
    local root_sh
    root_sh=$(cd "$BASEDIR/magisk" && ls *.sh 2>/dev/null | sort | tr '\n' ' ')
    # Leading space on purpose: the `case` below matches " $f " inside the list.
    local allowed=" customize.sh post-fs-data.sh service.sh uninstall.sh "
    local stray=""
    local f
    for f in $root_sh; do
        case "$allowed" in
            *" $f "*) ;;
            *) stray="$stray $f" ;;
        esac
    done
    if [ -n "$stray" ]; then
        echo " !! magisk/ root carries scripts that are not module entry points:$stray"
        echo "    libs belong in magisk/script/ (the module's scripts source from there)"
        exit 1
    fi
    for f in initsvc.sh libuperf.sh libcommon.sh libsysinfo.sh libcgroup.sh libpowercfg.sh \
             pathinfo.sh powercfg_main.sh powercfg_once.sh miui_migt.sh platform_special.sh \
             vtools_powercfg.sh webui.sh dfps.sh setup.sh uperf_watchdog.sh; do
        [ -f "$BASEDIR/magisk/script/$f" ] || { echo " !! magisk/script/$f is missing"; exit 1; }
    done
    echo "    scripts   : root = entry points only ($(echo $root_sh | wc -w) file(s)), libs in script/"

    # No third-party binaries in the module (§1: zero closed-source blobs, and now zero
    # vendored toolboxes too — busybox was removed in favour of the ROM's own toybox).
    # Both halves of that are asserted: the directory must be gone, and no script may
    # quietly reintroduce a dependency on it.
    if [ -e "$BASEDIR/magisk/bin/busybox" ]; then
        echo " !! magisk/bin/busybox is back; the module ships no toolbox (docs/records/shell-dependencies.md)"
        exit 1
    fi
    local bb_refs
    bb_refs=$(grep -rl "busybox/busybox" "$BASEDIR/magisk" 2>/dev/null | tr '\n' ' ')
    if [ -n "$bb_refs" ]; then
        echo " !! scripts still execute a bundled busybox: $bb_refs"
        exit 1
    fi
    echo "    binaries  : magisk/bin = uperf + libsfanalysis_rs.so only, no vendored toolbox"

    echo "    -> OK"
}

pack_uperf() {
    # `pack` does not build; a bare `sh build.sh pack` on a fresh clone would otherwise
    # fail inside `cp` with a path-only message.
    [ -f "$BINARY" ] || {
        echo " !! $BINARY is not built; nothing to pack"
        echo "    run: sh build.sh $BUILD_TYPE make pack"
        exit 1
    }
    build_webui
    echo ">>> Packing uperf-magisk.zip"
    rm -rf $STAGE_DIR
    mkdir -p $STAGE_DIR $PKG_DIR
    cp -a $BASEDIR/magisk/. $STAGE_DIR/
    cp -f $BINARY $STAGE_DIR/bin/uperf
    # M8: ship the new SfAnalysis injection library, replacing vendor's
    # closed-source `libsfanalysis.so`. The customization phase
    # (`magisk/customize.sh`) will `patchelf --add-needed` it onto
    # `/system/bin/surfaceflinger`.
    # The `sfanalysis` profile's output — NOT `release/`, which would ship the
    # panic=abort build of a library that is injected into surfaceflinger.
    local rs_so="$BASEDIR/rust/target/$ARM64_TARGET/sfanalysis/libsfanalysis_rs.so"
    if [ -f "$rs_so" ]; then
        # Only into the staging tree that gets zipped. `magisk/bin/` stays free of built
        # artifacts (see .gitignore); `check` asserts on the staged copy.
        cp -f "$rs_so" $STAGE_DIR/bin/libsfanalysis_rs.so
        echo "    -> bin/libsfanalysis_rs.so ($(stat -c %s "$rs_so") bytes)"
    fi
    cp -f $BASEDIR/LICENSE $BASEDIR/NOTICE $STAGE_DIR/
    # Remove the previous archive first. `zip -r` *updates* an existing archive:
    # it adds and replaces entries but never drops ones that are gone from the
    # staging tree — so every pack left its predecessor's hashed WebUI bundle
    # behind. Caught before publishing rc2: the zip carried four bundles, three
    # of them stale, one of them the build whose dfps tab rendered an empty list.
    # (`zip -FS` would also sync, but deleting is unambiguous.)
    rm -f $PKG_DIR/uperf-magisk.zip
    (cd $STAGE_DIR && zip -q -9 -r $PKG_DIR/uperf-magisk.zip .)
    # Nothing is mirrored into magisk/bin/: the module binary is a build product and the
    # staged tree is the only place it needs to exist. (It used to live there as tracked
    # source, which is why the vendor 1,461,512-byte binary could hide in plain sight.)
    echo "    -> $PKG_DIR/uperf-magisk.zip ($(stat -c %s $PKG_DIR/uperf-magisk.zip) bytes)"
    echo "    -> magisk/bin/uperf refreshed from $BINARY"
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
    # KernelSU/KernelSU-Next has no `magisk` binary; it installs modules with
    # `ksud module install`. Pick the installer first, then run one plain command: the
    # remote side gets a single `su -c '…'` argument, so nothing needs escaping here.
    local remote_cmd="magisk --install-module $REMOTE_DIR/uperf-magisk.zip"
    if $ADB shell "su -c 'command -v ksud'" >/dev/null 2>&1; then
        remote_cmd="ksud module install $REMOTE_DIR/uperf-magisk.zip"
    fi
    $ADB shell "su -c '$remote_cmd'"
    # Sync before the reboot. On f2fs (alioth: `fsync_mode=nobarrier`) an update staged
    # into modules_update and rebooted straight away came up with every module file
    # **all zero bytes at the right size** — metadata committed, data never flushed.
    # The module then looks installed (same names, same sizes) and does nothing, and its
    # service.sh cannot even start. One `sync` after the install prevents that window.
    $ADB shell sync
}

reboot_device() {
    echo ">>> Rebooting device"
    # Same reason as `install`: never reboot with module data still dirty in the page
    # cache, or the update can come back as zero-filled files.
    $ADB shell sync
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
        # pack before check: check asserts magisk/bin/uperf matches the build
        make_uperf
        pack_uperf
        check_uperf
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
