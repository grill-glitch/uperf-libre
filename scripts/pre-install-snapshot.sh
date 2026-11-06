#!/system/bin/sh
# Pre-install state: what is on the device now, so the post-boot comparison has a
# baseline (the 2026-10-11 incident came back as same-size all-NUL stubs).
set -u
OUT=/data/local/tmp/uperf-preinstall.txt
: > $OUT
M=/data/adb/modules/uperf
{
    echo "== uptime =="
    uptime
    echo "== installed module.prop =="
    cat $M/module.prop 2>/dev/null | head -8
    echo "== installed files =="
    find $M -type f 2>/dev/null | wc -l
    echo "== installed binary =="
    md5sum $M/bin/uperf 2>/dev/null
    ls -l $M/bin/uperf 2>/dev/null
    echo "== modules present =="
    ls /data/adb/modules 2>/dev/null
    echo "== any uperf process =="
    ps -A -o NAME | grep -c '^uperf$'
    echo "== governors =="
    for d in /sys/devices/system/cpu/cpufreq/policy*; do
        printf '%s %s\n' "$(basename $d)" "$(cat $d/scaling_governor 2>/dev/null)"
    done
    echo "== user dir =="
    ls -l /sdcard/Android/yc/uperf 2>/dev/null | head -12
} >> $OUT 2>&1
cat $OUT
