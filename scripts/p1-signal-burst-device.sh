#!/system/bin/sh
# P1 phase 2: a burst of process-directed SIGTERMs at the *worker* — the shape that used
# to run the teardown on an arbitrary thread while it held a lock. Expectation: one
# clean exit, the fake-root governor restored, and the supervisor still alive (it is
# only told to stop by its own signal; a worker death alone does not restart it).
set -u
T=/data/local/tmp/p1burst
BIN=$T/uperf
LOG=$T/daemon.log
CFG=$T/uperf.json
POL=$T/fakeroot/sys/devices/system/cpu/cpufreq/policy0
FAIL=0
note() { echo "$*"; }
bad() { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }

killall uperf 2>/dev/null
sleep 1
rm -rf $T
mkdir -p $POL
cp -f /data/local/tmp/uperf-p1 $BIN || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $BIN
echo schedutil > $POL/scaling_governor

cat > $CFG <<'EOF'
{
  "meta": {"name": "p1burst", "author": "audit"},
  "modules": {
    "cpu": {"enable": true, "powerModel": [
      {"efficiency": 115, "nr": 4, "typicalPower": 0.3, "typicalFreq": 1.8,
       "sweetFreq": 1.4, "plainFreq": 1.2, "freeFreq": 0.6}]},
    "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0, "trigger": 0.03,
                                   "gesture": 0.1, "switch": 0.4, "junk": 0.06}},
    "sysfs": {"enable": false, "knob": {}}
  },
  "initials": {"cpu": {"baseSampleTime": 0.05, "baseSlackTime": 0.05, "margin": 0.2}},
  "presets": {"balance": {"*": {}}}
}
EOF

cd $T
UPERF_FAKE_ROOT=$T/fakeroot UPERF_CPU_GOVERNOR=1 UPERF_LOG_MAX_BYTES=262144 \
  $BIN -o $LOG $CFG >/dev/null 2>&1
sleep 2

pids=$(ps -A -o PID,NAME | awk '$NF=="uperf"{print $1}')
supervisor=$(echo "$pids" | head -1)
worker=$(echo "$pids" | tail -1)
note "supervisor=$supervisor worker=$worker governor=$(cat $POL/scaling_governor)"
[ "$supervisor" != "$worker" ] || bad "expected two processes"
[ "$(cat $POL/scaling_governor)" = "userspace" ] || bad "governor was not armed"

# 20 process-directed SIGTERMs at the worker, back to back.
i=1
while [ $i -le 20 ]; do
    kill -TERM "$worker" 2>/dev/null
    i=$((i + 1))
done

n=0
while [ $n -lt 40 ]; do
    kill -0 "$worker" 2>/dev/null || break
    sleep 0.1
    n=$((n + 1))
done
note "worker gone after ~$((n)) ticks"

kill -0 "$worker" 2>/dev/null && bad "worker still alive"
[ "$(cat $POL/scaling_governor)" = "schedutil" ] || bad "governor=$(cat $POL/scaling_governor)"
[ "$(grep -c '^state=stopped' $T/uperf.state)" = "1" ] || bad "state not stopped"
caught=$(grep -c 'caught signal' $LOG)
[ "$caught" -ge 1 ] || bad "listener never ran"
note "caught=$caught tasks_stopped=$(grep -c "task '.*' stopped" $LOG)"
grep -E "caught signal|task '" $LOG | tail -6

# Now the supervisor, the way the module stops it.
kill -TERM "$supervisor" 2>/dev/null
n=0
while [ $n -lt 30 ]; do
    kill -0 "$supervisor" 2>/dev/null || break
    sleep 0.1
    n=$((n + 1))
done
left=$(ps -A -o NAME | grep -c '^uperf$')
note "after supervisor TERM: left=$left"
[ "$left" = "0" ] || bad "$left process(es) left"
echo "== summary: OK =="
[ "$FAIL" = "0" ] || exit 1
