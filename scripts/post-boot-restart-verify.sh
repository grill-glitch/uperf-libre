#!/system/bin/sh
# The path that could not be tested before the install: the module's own
# `webui.sh restart`, twice in a row. What must hold afterwards:
#   * two processes again, and the pids changed;
#   * `uperf.state` says running and its `pid=` is the *live* worker (the old bug was
#     `state=stopped` beside a running daemon after a fast restart);
#   * the stopped run left a real teardown in its log (`caught signal` + per-task stop
#     lines), not a tombstone;
#   * every governor is still `schedutil` (nothing armed, nothing left behind).
set -u
M=/data/adb/modules/uperf
U=/sdcard/Android/yc/uperf
FAIL=0
bad() { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }
pids() { ps -A -o PID,NAME | awk '$NF=="uperf"{print $1}' | tr '\n' ' '; }
state_of() { grep -E "^$1=" $U/uperf.state 2>/dev/null | cut -d= -f2; }
govs() { for d in /sys/devices/system/cpu/cpufreq/policy*; do printf '%s=%s ' "$(basename $d)" "$(cat $d/scaling_governor)"; done; }

echo "== before =="
echo "  pids: $(pids)"
echo "  state=$(state_of state) pid=$(state_of pid) takeover=$(state_of takeover)"
echo "  governors: $(govs)"

i=1
while [ $i -le 2 ]; do
    echo "== restart $i =="
    before=$(pids)
    out=$(sh $M/script/webui.sh restart 2>&1)
    echo "$out" | grep -E "restart\.|daemon\.count|daemon\.pids" | sed 's/^/  /'
    echo "$out" | grep -q "restart.ok=1" || bad "restart $i did not report ok"
    sleep 2

    after=$(pids)
    echo "  pids after: $after"
    [ "$before" != "$after" ] || bad "restart $i: pids unchanged ($after)"
    n=$(echo "$after" | wc -w)
    [ "$n" = "2" ] || bad "restart $i: $n process(es), expected 2"

    st=$(state_of state)
    spid=$(state_of pid)
    echo "  state=$st pid=$spid"
    [ "$st" = "running" ] || bad "restart $i: state=$st"
    # The live worker is the last pid in ps order; the status must name it, not the
    # previous generation.
    worker=$(echo "$after" | awk '{print $NF}')
    [ "$spid" = "$worker" ] || bad "restart $i: status pid=$spid but live worker=$worker"

    echo "  governors: $(govs)"
    echo "$(govs)" | grep -q userspace && bad "restart $i: a policy is still in userspace"

    # The teardown of the run just stopped is in the archive: `uperf_start` moves the
    # live log aside and the previous worker's last lines went to the fd it still held.
    # Assert here — the *next* restart rotates `.bak` again, clobbering this evidence.
    grep -q "caught signal" $U/uperf_log.txt.bak \
        || bad "restart $i: no 'caught signal' line in the archive"
    grep -q "daemon stopped" $U/uperf_log.txt.bak \
        || bad "restart $i: no 'daemon stopped' line in the archive"
    grep -qi tombstone $U/uperf_log.txt.bak && bad "restart $i: a tombstone was printed"
    i=$((i + 1))
done

echo "== teardown lines from the last two runs =="
cat $U/uperf_log.txt.bak $U/uperf_log.txt 2>/dev/null \
    | grep -E "caught signal|task '[a-z-]+' stopped|daemon stopped|daemon started" | tail -12 | sed 's/^/  /'

echo "== webui.sh status after two restarts =="
sh $M/script/webui.sh status 2>&1 | grep -E "daemon\.count|daemon\.state|watchdog\.state|governor\.takeover|log\.lines" | sed 's/^/  /'

echo "== RESULT: $FAIL failure(s) =="
[ "$FAIL" = "0" ] || exit 1
