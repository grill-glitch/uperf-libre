#!/system/bin/sh
# ⑤ priority e2e: with the injected hint fresh the daemon reports source=hint and does
# NOT poll binder; with no hint it takes the fps leg; when the hint goes stale it falls
# back again. Teardown removes only uperf pids that were not running before.
T=/data/local/tmp/sfb-prio
OUT=$T/out.log
PS=/system/bin/ps
say() { echo "$@" >> "$OUT"; }

BEFORE=$($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')
rm -rf "$T"; mkdir -p "$T/user" "$T/root"; : > "$OUT"
say "installed uperf before: $BEFORE"

cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
cp /data/local/tmp/uperf_sfb "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! copy failed"
HINT="$T/user/sfanalysis.hint"
rm -f "$HINT"

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1; sleep 2

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_TICK_MS=1000 \
UPERF_SF_BINDER_HINT_STALE_MS=2000 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &

# A: no hint file at all -> the fps leg
sleep 4
say "A no-hint  frames.state: $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"

# B: keep the hint fresh for ~6 s -> the hint leg, and binder polling must stop
i=0
while [ $i -lt 6 ]; do
    printf '\004' >"$HINT" 2>/dev/null
    sleep 1
    i=$((i + 1))
done
say "B fresh    frames.state: $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"
B_LINES=$(grep -c "fps=" "$T/user/daemon_log.txt" 2>/dev/null)
say "B fps-lines-so-far: $B_LINES"

# C: stop writing; past the 2 s staleness it must fall back on its own
sleep 6
say "C stale    frames.state: $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"

say "--- log: source transitions + fps lines ---"
grep -E "frame source ->|sf-binder layer=|--- " "$T/user/daemon_log.txt" 2>/dev/null | tail -18 >>"$OUT"
say "--- fps lines after C ---"
grep -c "fps=" "$T/user/daemon_log.txt" 2>/dev/null >>"$OUT"

for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
    case " $BEFORE " in
    *" $p "*) ;;
    *) kill -TERM "$p" 2>/dev/null ;;
    esac
done
sleep 2
say "after teardown: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"
rm -f "$HINT"
cat "$OUT"
