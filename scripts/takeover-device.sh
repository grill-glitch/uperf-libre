#!/system/bin/sh
# Real-device frequency takeover, and the disarm that has to survive it.
#
# The fake-root runs prove the lifecycle but write into a scratch tree; this one writes
# the *real* `scaling_governor` / `scaling_setspeed`, which is the state that cannot be
# recovered from in-process once lost. Sequence:
#   1. stop the module's daemon + watchdog (so nothing restarts what we start);
#   2. run the built binary with `UPERF_CPU_GOVERNOR=1` and the module's own config;
#   3. assert all three policies are `userspace` and a target was published;
#   4. `killall uperf` — the module's own teardown;
#   5. assert all three are back to their recorded original, the originals were written
#      to `orig_governor.txt` before the takeover, and `state=stopped takeover=on`;
#   6. put the module back the way it was (`webui.sh restart`).
set -u
T=/data/local/tmp/takeover
M=/data/adb/modules/uperf
U=/sdcard/Android/yc/uperf
FAIL=0
bad() { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }
note() { echo "$*"; }
governors() { for d in /sys/devices/system/cpu/cpufreq/policy*; do printf '%s=%s ' "$(basename $d)" "$(cat $d/scaling_governor)"; done; }
orig_all() { for d in /sys/devices/system/cpu/cpufreq/policy*; do cat $d/scaling_governor; done | sort -u | tr '\n' ',' ; }

rm -rf $T
mkdir -p $T
cp -f /data/local/tmp/uperf-takeover $T/uperf || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $T/uperf

note "== stop the module's own supervision =="
# There is no `webui.sh stop` (status|info|all|set-preset|log|export|restart), and
# `libuperf.sh`/`pathinfo.sh` resolve their siblings from `$0`, so sourcing them from
# this script would compute the wrong module path. Run a one-shot file *inside*
# `script/` instead — that is what `$0` has to be for `pathinfo.sh`'s
# `readlink -f "$0"` to yield the module root — and delete it immediately.
STOPPER=$M/script/.audit-stop.sh
cat > $STOPPER <<'EOS'
#!/system/bin/sh
. "$(dirname "$(readlink -f "$0")")/libuperf.sh"
uperf_stop
EOS
sh $STOPPER 2>&1 | tail -3
rm -f $STOPPER
sleep 2
left=$(ps -A -o NAME | grep -c '^uperf$')
note "  processes: $left"
note "  governors: $(governors)"
# If the module daemon is still up it would share the log, the state file and the
# takeover with ours; the assertions would then describe two processes, not one.
[ "$left" = "0" ] || bad "the module's daemon did not stop ($left processes left)"
before=$(orig_all)
note "  originals (unique): $before"

# Anything left armed from an earlier attempt would poison the "before" snapshot.
case "$before" in
    *userspace*) bad "a policy is already in userspace before the test"; exit 1 ;;
esac

note "== start the daemon with the takeover enabled (real sysfs) =="
cd $T
UPERF_CPU_GOVERNOR=1 UPERF_LOG_MAX_BYTES=262144 \
  $T/uperf -o $T/daemon.log $U/uperf.json >/dev/null 2>&1
sleep 6
note "  governors: $(governors)"
note "  speeds: $(for d in /sys/devices/system/cpu/cpufreq/policy*; do printf '%s=%s ' "$(basename $d)" "$(cat $d/scaling_setspeed 2>/dev/null)"; done)"
note "  recorded: $(tr '\n' ';' < $U/orig_governor.txt 2>/dev/null)"
grep -E "cpu governor armed|governor idle" $T/daemon.log | tail -2 | sed 's/^/  /'

armed=$(governors)
echo "$armed" | grep -q "policy0=userspace" || bad "policy0 did not arm: $armed"
[ "$(echo "$armed" | grep -o userspace | wc -l)" = "3" ] || bad "not all policies armed: $armed"
[ -s $U/orig_governor.txt ] || bad "orig_governor.txt is empty: nothing could restore this"
grep -q "userspace" $U/orig_governor.txt && bad "orig_governor.txt recorded 'userspace' as an original"
grep -qE "^state=running" $U/uperf.state || bad "state is not running while armed"
grep -qE "^takeover=on" $U/uperf.state || bad "state does not report takeover=on"

note "== teardown (killall uperf, the module's own path) =="
killall uperf 2>/dev/null
n=0
while [ $n -lt 40 ]; do
    [ "$(ps -A -o NAME | grep -c '^uperf$')" = "0" ] && break
    sleep 0.1
    n=$((n + 1))
done
sleep 0.5
after=$(governors)
note "  after $n ticks: $after"
note "  recorded: $(tr '\n' ';' < $U/orig_governor.txt 2>/dev/null)"
echo "$after" | grep -q userspace && bad "a policy is STILL in userspace after a graceful stop"
[ "$(orig_all)" = "$before" ] || bad "governors not restored: before=$before after=$(orig_all)"
grep -qE "^state=stopped" $U/uperf.state || bad "state is not stopped"
grep -qE "^takeover=on" $U/uperf.state || bad "the stop record lost takeover=on"
grep -E "disarmed|caught signal|task 'cpu' stopped" $T/daemon.log | tail -4 | sed 's/^/  /'
grep -q "caught signal" $T/daemon.log || bad "the listener did not handle the signal"

note "== restore the module's normal supervision =="
sh $M/script/webui.sh restart 2>&1 | grep -E "restart\.|daemon\." | tail -4
sleep 3
note "  processes: $(ps -A -o NAME | grep -c '^uperf$')"
note "  governors: $(governors)"
note "  state: $(grep -E '^(state|takeover|armed)=' $U/uperf.state | tr '\n' ' ')"

echo "== RESULT: $FAIL failure(s) =="
[ "$FAIL" = "0" ] || exit 1
