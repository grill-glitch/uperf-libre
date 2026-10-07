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
# Per-module customization. Runs at install time (Magisk/KSU module install).
#
# Order matters:
#   1. Seed dfps.txt BEFORE setup.sh runs — see comment below.
#   2. Run setup.sh (this copies the matched SoC config out and then
#      `rm -rf`s $MODULE_PATH/config/, which would wipe our dfps default).
#   3. Patchelf surfaceflinger to pull in libsfanalysis_rs.so. This MUST
#      happen before the device reboots (the next boot is when SF loads).

SKIPUNZIP=0

# Seed dfps.txt BEFORE setup.sh runs.
#
# Order matters: setup.sh -> install_uperf() does `rm -rf $MODULE_PATH/config`
# after copying the matched SoC config out, so `config/dfps.default.txt` is gone
# by the time setup.sh returns. Seeding after it would silently never fire, and
# a fresh install would boot with no dfps.txt (dfps logs "Rust: dfps disabled"
# and the refresh-rate tab shows nothing).
#
# Only on a fresh install: an existing dfps.txt belongs to the user. That also
# covers someone migrating from the separate yc_dfps module — their own rules
# stay put, which AGENT.md §12.2 accepts as the cost of the embedded mode.
USER_PATH_DFPS="/sdcard/Android/yc/uperf"
mkdir -p "$USER_PATH_DFPS" 2>/dev/null
if [ ! -e "$USER_PATH_DFPS/dfps.txt" ] && [ -f "$MODPATH/config/dfps.default.txt" ]; then
    cp -f "$MODPATH/config/dfps.default.txt" "$USER_PATH_DFPS/dfps.txt"
    chmod 644 "$USER_PATH_DFPS/dfps.txt"
fi

sh $MODPATH/script/setup.sh
[ "$?" != "0" ] && abort

# M8: SfAnalysis injection. Replace vendor's closed-source libsfanalysis.so
# (AGENT.md §1 / §12.1) with our Rust-built libsfanalysis_rs.so.
#
# Vendor's mechanism is documented in
#   https://github.com/yc9559/surfaceflinger-analysis/
# and uses `patchelf --add-needed` on /system/bin/surfaceflinger. We do
# the same; on Android, surfaceflinger is the canonical patchelf target
# and the dynamic loader pulls in libsfanalysis_rs.so on next boot.
#
# The patchelf binary is brought by this module under `bin/patchelf`; if
# it isn't present yet (legacy layout), fall back to the system patchelf
# when available. Either way, the resulting SF is dropped into
# $MODPATH/system/bin/surfaceflinger so Magisk's overlay mount picks it up
# on next boot.
PATCHELF=""
for candidate in "$MODPATH/bin/patchelf" "/system/bin/patchelf" "$(command -v patchelf 2>/dev/null)"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ]; then
        PATCHELF="$candidate"
        break
    fi
done

if [ -n "$PATCHELF" ] && [ -f "$MODPATH/bin/libsfanalysis_rs.so" ]; then
    SF_SRC="/system/bin/surfaceflinger"
    SF_OUT="$MODPATH/system/bin/surfaceflinger"
    mkdir -p "$MODPATH/system/bin" 2>/dev/null
    if [ -f "$SF_SRC" ] && [ ! -f "$SF_OUT" ]; then
        echo "- M8: patchelf surfaceflinger to require libsfanalysis_rs.so"
        cp -f "$SF_SRC" "$SF_OUT" 2>/dev/null && \
            "$PATCHELF" --add-needed libsfanalysis_rs.so "$SF_OUT" 2>/dev/null && \
            chmod 755 "$SF_OUT" && \
            chcon "$(ls -Zl "$SF_SRC" | cut -d' ' -f5)" "$SF_OUT" 2>/dev/null
        # Tell the daemon which path to read hint bytes from. AGENT.md §7.4
        # hard-codes the hint path next to uperf.json, so this is informational
        # only — kept here as a single point of truth for diagnostics.
        USER_PATH_HINT="/sdcard/Android/yc/uperf"
        export UPERF_SF_HINT_FILE="$USER_PATH_HINT/sfanalysis.hint"
    fi
else
    echo "- M8: skipping surfaceflinger patchelf (no patchelf or no libsfanalysis_rs.so)"
fi
