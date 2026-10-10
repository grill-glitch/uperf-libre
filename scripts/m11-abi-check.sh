#!/system/bin/sh
# ⑩ ABI guard e2e (device). Exercises the real helper in libsysinfo.sh on the real ABI
# and, via the UPERF_FAKE_ABI seam, on a 32-bit one -- which is the only way to reach the
# refusal path without a 32-bit device.
#
# Needs: /data/local/tmp/abi/libsysinfo.sh (the repo copy).
LIB=/data/local/tmp/abi/libsysinfo.sh
if [ ! -f "$LIB" ]; then
    echo "!! missing $LIB"
    exit 2
fi
BASEDIR=/data/local/tmp/abi
. "$LIB"

echo "=== real device ==="
echo "ro.product.cpu.abi   : $(getprop ro.product.cpu.abi)"
echo "abilist              : $(getprop ro.product.cpu.abilist)"
echo "module_abi()         : $(module_abi)"
echo "abi_supported()      : $(abi_supported)"
echo "is_aarch64()         : $(is_aarch64)"
require_aarch64
echo "require_aarch64 rc   : $?"

echo
echo "=== UPERF_FAKE_ABI=armeabi-v7a (refusal path) ==="
UPERF_FAKE_ABI=armeabi-v7a
export UPERF_FAKE_ABI
echo "module_abi()         : $(module_abi)"
echo "abi_supported()      : $(abi_supported)"
echo "is_aarch64()         : $(is_aarch64)"
require_aarch64
echo "require_aarch64 rc   : $?  (non-zero -> setup.sh aborts the install here)"

echo
echo "=== what setup.sh does with that rc ==="
grep -n -B1 -A1 "require_aarch64" $BASEDIR/setup.sh 2>/dev/null || echo "(setup.sh copy not pushed)"

echo
echo "=== installed module: is the guard in the shipped tree? ==="
for f in /data/adb/modules/uperf/script/libsysinfo.sh /data/adb/modules/uperf/script/setup.sh; do
    [ -f "$f" ] && echo "$f: $(grep -c 'require_aarch64\|abi_supported' "$f" 2>/dev/null) reference(s)"
done
