#!/system/bin/sh
# ④ device e2e: the real built daemon reads <USER_PATH>/foreground.txt (written by the
# helper) and logs it as topapp.pkgName across am start switches, all under a fake root.
# Teardown is by recorded PID so the installed daemon is never touched.
T=/data/local/tmp/fg-e2e
OUT=$T/out.log
say() { echo "$@" >> "$OUT"; }
PS=/system/bin/ps

rm -rf "$T"
mkdir -p "$T/user" "$T/root"
: > "$OUT"

say "installed uperf before: $($PS -A -o PID,NAME 2>/dev/null | grep -w uperf | tr '\n' ' ')"

cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{"meta":{"name":"e2e","author":"agent"},"modules":{},"presets":{},"scenes":{}}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
mkdir -p "$T/root/sys/devices/system/cpu/cpufreq/policy0"
printf 'schedutil\n' > "$T/root/sys/devices/system/cpu/cpufreq/policy0/scaling_governor"

cp /data/local/tmp/uperf_test "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! binary copy failed"

# --- helper: writes the top-app file into the config dir (USER_PATH) ---
CLASSPATH=/data/local/tmp/foreground.jar app_process /system/bin ForegroundHelper "$T/user/foreground.txt" 1000 \
    </dev/null >/dev/null 2>"$T/helper.err" &
HPID=$!
sleep 3
say "helper pid=$HPID alive=$(kill -0 $HPID 2>/dev/null && echo yes || echo no) file='$(cat "$T/user/foreground.txt" 2>/dev/null)'"

# --- the daemon under test: fake root, dry-run sched, takeover off (no UPERF_CPU_GOVERNOR) ---
UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_FOREGROUND_TICK_MS=300 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &
DPID=$!
sleep 4
say "daemon pid=$DPID alive=$(kill -0 $DPID 2>/dev/null && echo yes || echo no)"
say "tree: $($PS -A -o PID,PPID,NAME 2>/dev/null | grep -wE "uperf|$DPID" | tr '\n' ' ')"

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1; sleep 4
input keyevent KEYCODE_HOME 2>/dev/null; sleep 4
am start -n com.android.documentsui/.files.FilesActivity >/dev/null 2>&1; sleep 4

say "helper file at end: '$(cat "$T/user/foreground.txt" 2>/dev/null)'"
say "--- daemon log: foreground source + topapp lines ---"
grep -E "foreground helper file source|topapp\.pkgName" "$T/user/daemon_log.txt" 2>/dev/null | tail -40 >> "$OUT"
say "--- daemon log tail ---"
tail -6 "$T/user/daemon_log.txt" 2>/dev/null >> "$OUT"

# --- teardown: only OUR pids (worker is a child of DPID) ---
KIDS=$($PS -A -o PID,PPID 2>/dev/null | awk -v p="$DPID" '$2==p{print $1}' | tr '\n' ' ')
say "our tree pids: daemon=$DPID kids=$KIDS"
kill -TERM $KIDS 2>/dev/null
kill -TERM "$DPID" 2>/dev/null
kill "$HPID" 2>/dev/null
sleep 2
say "after teardown: installed uperf: $($PS -A -o PID,NAME 2>/dev/null | grep -w uperf | tr '\n' ' ')"
cat "$OUT"
