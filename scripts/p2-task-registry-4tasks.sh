#!/system/bin/sh
# P2 device check, phase 2: four tasks (cpu, sched, record, watch) started by the
# daemon, and the stop order the registry derives — governor first, then reverse
# start order. Everything is redirected (`UPERF_FAKE_ROOT`, `UPERF_SCHED_DRY_RUN`)
# so no kernel state is touched.
set -u
T=/data/local/tmp/p2check2
BIN=$T/uperf
LOG=$T/daemon.log
CFG=$T/uperf.json

rm -rf $T
mkdir -p $T/fakeroot/sys/devices/system/cpu/cpufreq/policy0
cp -f /data/local/tmp/uperf-p2 $BIN || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $BIN
echo schedutil > $T/fakeroot/sys/devices/system/cpu/cpufreq/policy0/scaling_governor
echo "600000 1200000 1800000" > $T/fakeroot/sys/devices/system/cpu/cpufreq/policy0/scaling_available_frequencies

cat > $CFG <<'EOF'
{
  "meta": {"name": "p2check2", "author": "audit"},
  "modules": {
    "cpu": {"enable": true, "powerModel": [
      {"efficiency": 115, "nr": 4, "typicalPower": 0.3, "typicalFreq": 1.8,
       "sweetFreq": 1.4, "plainFreq": 1.2, "freeFreq": 0.6}]},
    "sched": {"enable": true, "cpumask": {"all": [0,1]},
              "affinity": {"ui": {"bg": "all"}}, "prio": {"ui": {"bg": -1}},
              "rules": [{"name": "r", "regex": "uperf", "pinned": false,
                         "rules": [{"k": ".", "ac": "ui", "pc": "ui"}]}]},
    "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0, "trigger": 0.03,
                                   "gesture": 0.1, "switch": 0.4, "junk": 0.06}},
    "sysfs": {"enable": false, "knob": {}}
  },
  "initials": {"cpu": {"baseSampleTime": 0.2, "baseSlackTime": 0.2, "margin": 0.2}},
  "presets": {"balance": {"*": {}}}
}
EOF

cd $T
UPERF_FAKE_ROOT=$T/fakeroot UPERF_CPU_GOVERNOR=1 UPERF_SCHED_DRY_RUN=1 \
  UPERF_RECORD=1 UPERF_LOG_MAX_BYTES=262144 \
  $BIN -o $LOG $CFG >/dev/null 2>&1
echo "launched rc=$?"
sleep 6

echo "== started =="
grep -E "daemon started|task .* stopped|cpu governor|context scheduler|recorder enabled|watcher started" $LOG | tail -8

PID=$(ps -A -o PID,NAME | awk '$NF=="uperf"{print $1}' | head -1)
echo "== stopping supervisor $PID =="
kill -TERM $PID
sleep 4

echo "== stop order =="
grep -nE "task '[a-z-]*' (stopped|did not stop)|daemon stopped" $LOG | tail -8
echo "== still running =="
ps -A -o PID,STAT,NAME | awk '$NF=="uperf"'
echo "== state =="
grep -E "^state=|^armed=|^policies=" $T/uperf.state 2>/dev/null
