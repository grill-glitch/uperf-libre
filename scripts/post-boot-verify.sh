#!/system/bin/sh
# Post-boot verification for the refreshed module: files survived the boot intact (the
# 2026-10-11 incident came back as same-size all-NUL stubs), the daemon was started by
# service.sh, the watchdog is alive, and `webui.sh restart` — the module's own teardown,
# which is the path that used to leave the governor armed — hands over cleanly.
set -u
M=/data/adb/modules/uperf
U=/sdcard/Android/yc/uperf
MAN=/data/local/tmp/uperf-zip-manifest.txt
FAIL=0
bad() { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }

echo "== boot =="
uptime
echo "boot reason: $(getprop sys.boot.reason)"

echo "== module =="
[ -d $M ] || bad "module dir missing"
[ -f $M/disable ] && bad "module is DISABLED"
[ -d /data/adb/modules_update/uperf ] && bad "still staged in modules_update"
grep -E '^version=' $M/module.prop
echo "files: $(find $M -type f | wc -l)"

echo "== manifest compare =="
mism=0
while read -r want path; do
    [ -n "${path:-}" ] || continue
    [ -e "$M/$path" ] || continue      # customize.sh + seeded configs are expected absent
    got=$(md5sum "$M/$path" 2>/dev/null | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "MISMATCH $path want=$want got=${got:-missing}"
        mism=$((mism + 1))
    fi
done < $MAN
echo "mismatches: $mism"
[ "$mism" = "0" ] || bad "$mism file(s) differ from the zip"

echo "== all-NUL scan =="
nul=0
for f in $(find $M -type f); do
    if [ -s "$f" ] && [ "$(tr -d '\000' < "$f" | wc -c)" = "0" ]; then
        echo "ALL-NUL $f"
        nul=$((nul + 1))
    fi
done
echo "all-nul: $nul"
[ "$nul" = "0" ] || bad "$nul file(s) are all-NUL (f2fs loss)"

echo "== daemon =="
sleep 3
pids=$(ps -A -o PID,NAME | awk '$NF=="uperf"{print $1}' | tr '\n' ' ')
echo "pids: $pids"
[ -n "$pids" ] || bad "no uperf process after boot"
grep -E '^(state|takeover|armed|pid|config)=' $U/uperf.state 2>/dev/null | sed 's/^/  /'
[ "$(grep -c '^state=running' $U/uperf.state 2>/dev/null)" = "1" ] || bad "state is not running"

echo "== watchdog =="
sed -n '1,6p' $U/uperf_watchdog.state 2>/dev/null | sed 's/^/  /'

echo "== binary identity =="
echo "  md5 $(md5sum $M/bin/uperf | cut -d' ' -f1) (build d54690cfc05b2d3803da42a0364649fb)"

echo "== config =="
ls -l $U/uperf.json $U/uperf.json.bak $U/uperf.json.default 2>/dev/null | sed 's/^/  /'
head -c 120 $U/uperf.json; echo
echo "  preset file: '$(cat $U/cur_powermode.txt 2>/dev/null)'"

echo "== governors (nothing should be armed right after boot) =="
for d in /sys/devices/system/cpu/cpufreq/policy*; do
    printf '  %s %s\n' "$(basename $d)" "$(cat $d/scaling_governor 2>/dev/null)"
done

echo "== webui.sh status =="
sh $M/script/webui.sh status 2>&1 | sed 's/^/  /'

echo "== log tail =="
tail -8 $U/uperf_log.txt 2>/dev/null | sed 's/^/  /'

echo "== RESULT: $FAIL failure(s) =="
[ "$FAIL" = "0" ] || exit 1
