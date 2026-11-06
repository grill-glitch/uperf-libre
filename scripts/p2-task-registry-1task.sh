#!/system/bin/sh
# P2 device check: the daemon's task registry and the ordered stop, on the real
# binary, under a fake root (so no kernel knob is touched).
set -u
T=/data/local/tmp/p2check
BIN=$T/uperf
LOG=$T/daemon.log
USER_PATH=/sdcard/Android/yc/uperf
CFG=$T/uperf.json

rm -rf $T
mkdir -p $T/fakeroot
cp -f /data/local/tmp/uperf-p2 $BIN 2>/dev/null || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $BIN

# Minimal config: one preset, sysfs disabled (the shipped one has enable=false too).
cat > $CFG <<'EOF'
{
  "meta": {"name": "p2check", "author": "audit"},
  "modules": {
    "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0, "trigger": 0.03,
                                   "gesture": 0.1, "switch": 0.4, "junk": 0.06}},
    "sysfs": {"enable": false, "knob": {}}
  },
  "initials": {"cpu": {"baseSampleTime": 0.1, "baseSlackTime": 0.1, "margin": 0.2}},
  "presets": {"balance": {"*": {}}}
}
EOF
echo balance > $USER_PATH/cur_powermode.txt 2>/dev/null

cd $T
UPERF_FAKE_ROOT=$T/fakeroot UPERF_LOG_MAX_BYTES=262144 $BIN -o $LOG $CFG >/dev/null 2>&1
echo "launched rc=$?"
sleep 5

echo "== processes =="
ps -A -o PID,STAT,NAME | awk '$NF=="uperf"'

echo "== daemon started line =="
grep -E "daemon started|task '" $LOG | tail -12

echo "== stop =="
PID=$(ps -A -o PID,NAME | awk '$NF=="uperf"{print $1}' | head -1)
echo "supervisor pid=$PID"
kill -TERM $PID
sleep 3

echo "== after stop =="
ps -A -o PID,STAT,NAME | awk '$NF=="uperf"'
echo "-- stop log --"
grep -E "task '|daemon stopped|uperf_rs_stop" $LOG | tail -14
echo "-- state file --"
sed -n '1,12p' $T/uperf.state 2>/dev/null | grep -E "state=|armed=|pid="

echo "== tail =="
tail -5 $LOG
