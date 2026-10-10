#!/system/bin/sh
# ⑤ takeover e2e: the injected hint is present for a moment (so the FSM goes to a
# non-idle scene), then goes stale — and the frame leg, which measures that nothing is
# being drawn, is what moves the scene back to idle. Teardown removes only new pids.
T=/data/local/tmp/sfb-take
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
input keyevent KEYCODE_HOME 2>/dev/null; sleep 2
# resolve the launcher buffer layer (the InputSink layers carry no frames)
L=$(dumpsys SurfaceFlinger --list 2>/dev/null | grep -m1 -E 'RequestedLayerState\{com.android.launcher3/')
L=$(echo "$L" | sed 's/^RequestedLayerState{//; s/ .*//')
say "layer='$L'"

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_LAYER="$L" \
UPERF_SF_BINDER_TICK_MS=1000 \
UPERF_SF_BINDER_WINDOW_MS=1000 \
UPERF_SF_BINDER_HINT_STALE_MS=2000 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &
sleep 4
say "A no hint yet  : $(grep -E 'sched scene=' "$T/user/daemon_log.txt" | tail -1)"
say "A frames.state : $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"

# make the injected hint say 'touch' — the FSM must leave idle
printf '\004' >"$HINT" 2>/dev/null
sleep 2
say "B hint=touch   : $(grep -E 'SfAnalysis hint|sched scene=' "$T/user/daemon_log.txt" | tail -2 | tr '\n' '|')"

# stop refreshing: past 2 s it is stale, the fps leg takes over and finds nothing drawn
sleep 6
say "C after stale  : $(grep -E 'sf-binder frame hint|frame source ->|sched scene=' "$T/user/daemon_log.txt" | tail -7 | tr '\n' '|')"
say "C frames.state : $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"

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
