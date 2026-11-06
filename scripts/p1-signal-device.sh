#!/system/bin/sh
# P1 device check: teardown is handled on a `sigwait` thread, so a SIGTERM that lands
# while the daemon is mid-tick cannot re-enter the stop path. This runs the real
# binary eight times with the governor armed under a fake root and stops each run the
# way the module itself does — `killall uperf`, which delivers SIGTERM to the
# supervisor *and* the worker, the exact shape that used to deadlock on the log mutex —
# then asserts: exit inside 3 s, governor disarmed, status `stopped`, nothing left.
set -u
T=/data/local/tmp/p1sig
BIN=$T/uperf
LOG=$T/daemon.log
CFG=$T/uperf.json
POL=$T/fakeroot/sys/devices/system/cpu/cpufreq/policy0
FAIL=0
RUNS=8

rm -rf $T
mkdir -p $POL
cp -f /data/local/tmp/uperf-p1 $BIN || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $BIN

cat > $CFG <<'EOF'
{
  "meta": {"name": "p1sig", "author": "audit"},
  "modules": {
    "cpu": {"enable": true, "powerModel": [
      {"efficiency": 115, "nr": 4, "typicalPower": 0.3, "typicalFreq": 1.8,
       "sweetFreq": 1.4, "plainFreq": 1.2, "freeFreq": 0.6}]},
    "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0, "trigger": 0.03,
                                   "gesture": 0.1, "switch": 0.4, "junk": 0.06}},
    "sysfs": {"enable": false, "knob": {}}
  },
  "initials": {"cpu": {"baseSampleTime": 0.1, "baseSlackTime": 0.1, "margin": 0.2}},
  "presets": {"balance": {"*": {}}}
}
EOF

real_governor() { cat /sys/devices/system/cpu/cpufreq/policy0/scaling_governor 2>/dev/null; }
before_gov="$(real_governor)"

i=1
while [ $i -le $RUNS ]; do
    echo "schedutil" > $POL/scaling_governor
    rm -f $POL/scaling_setspeed
    : > $LOG
    cd $T
    UPERF_FAKE_ROOT=$T/fakeroot UPERF_CPU_GOVERNOR=1 UPERF_LOG_MAX_BYTES=262144 \
      $BIN -o $LOG $CFG >/dev/null 2>&1
    sleep 2
    armed="$(cat $POL/scaling_governor 2>/dev/null)"

    # The module's own teardown (`libuperf.sh`): SIGTERM to every process named uperf.
    killall uperf 2>/dev/null

    # Wait up to 3 s for both processes to go.
    n=0
    left=1
    while [ $n -lt 30 ]; do
        left=$(ps -A -o NAME | grep -c '^uperf$')
        [ "$left" = "0" ] && break
        sleep 0.1
        n=$((n + 1))
    done

    restored="$(cat $POL/scaling_governor 2>/dev/null)"
    stopped=$(grep -c "^state=stopped" $T/uperf.state 2>/dev/null)
    caught=$(grep -c "caught signal" $LOG 2>/dev/null)
    oom=$(grep -c "did not stop" $LOG 2>/dev/null)

    verdict=ok
    [ "$armed" = "userspace" ] || verdict="FAIL(never armed)"
    [ "$left" = "0" ] || verdict="FAIL($left processes left)"
    [ "$restored" = "schedutil" ] || verdict="FAIL(governor=$restored)"
    [ "$stopped" = "1" ] || verdict="FAIL(state not stopped)"
    [ "$caught" -ge 1 ] || verdict="FAIL(listener never ran)"
    [ "$oom" = "0" ] || verdict="FAIL(task did not stop)"

    echo "run $i: armed=$armed restored=$restored left=$left caught=$caught -> $verdict"
    [ "$verdict" = "ok" ] || FAIL=$((FAIL + 1))
    i=$((i + 1))
done

after_gov="$(real_governor)"
echo "real policy0 governor: before=$before_gov after=$after_gov (must be untouched)"
[ "$before_gov" = "$after_gov" ] || FAIL=$((FAIL + 1))

echo "== last run log (tail) =="
tail -6 $LOG
echo "== summary: $((RUNS - FAIL))/$RUNS clean =="
[ "$FAIL" = "0" ] || exit 1
