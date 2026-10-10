#!/system/bin/sh
# ⑪ (AppOpt primary leg) e2e, pinned: with `UPERF_SF_UPROBE_PID` the probe's target is
# fixed, so this measures ⑪'s counting itself rather than ④'s foreground reader. Frames
# come from up-only scrolls of a freshly started Settings list (alternating swipes pull
# down Settings' search screen, which then has nothing to scroll). An independent PMU
# reading of the RenderThread over the same window says whether frames really flowed.
T=/data/local/tmp/up-e2e
OUT=$T/out.log
PS=/system/bin/ps
LOG=$T/user/daemon_log.txt
say() { echo "$@" >> "$OUT"; }

BEFORE=$($PS -A -o PID,NAME 2>/dev/null | awk '$2 ~ /^uperf/{print $1}' | tr '\n' ' ')
rm -rf "$T"; mkdir -p "$T/user" "$T/root"; : > "$OUT"
say "uperf pids before: [$BEFORE]"
cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
cp /data/local/tmp/uperf_up "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! copy failed"

# a display timeout mid-test silently stops all frame production, which then looks like a
# broken probe; hold the screen on for the duration and restore it on the way out
svc power stayon true 2>/dev/null
input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
input keyevent KEYCODE_HOME 2>/dev/null; sleep 1
am force-stop com.android.settings 2>/dev/null
am force-stop com.android.settings.intelligence 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1; sleep 3
input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1

FPID=$($PS -A -o PID,NAME 2>/dev/null | awk '$2 == "com.android.settings" {print $1; exit}')
RT=$(for t in /proc/$FPID/task/*; do grep -q RenderThread $t/comm 2>/dev/null && basename $t; done | head -1)
say "target app pid=$FPID RenderThread=$RT"
[ -n "$FPID" ] || { say "!! no com.android.settings process"; }

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_TICK_MS=1000 \
UPERF_SF_BINDER_WINDOW_MS=1000 \
UPERF_SF_UPROBE=1 \
UPERF_SF_UPROBE_PID="$FPID" \
UPERF_SF_UPROBE_WINDOW_MS=1000 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$LOG" </dev/null >/dev/null 2>&1 &

n=0
while [ $n -lt 20 ]; do
    grep -q 'sf-uprobe attached' "$LOG" 2>/dev/null && break
    sleep 1
    n=$((n + 1))
done
say "attach      : $(grep -E 'sf-uprobe attached' "$LOG" | head -1)"

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
# up-only scrolls: the list keeps moving, so frames keep coming
(
    i=0
    while [ $i -lt 45 ]; do
        input swipe 540 1700 540 500 150 2>/dev/null
        sleep 0.35
        i=$((i + 1))
    done
) >/dev/null 2>&1 &
SW=$!
sleep 7
say "independent : $(timeout 20 /data/local/tmp/uprobe-count pmu /system/lib64/libgui.so 0xf43ec $RT 5 2>&1)"
sleep 2
kill $SW 2>/dev/null

say "daemon fps  : $(grep -E 'sf-uprobe .*fps=' "$LOG" | tail -8 | tr '\n' '|')"
say "frames.state: $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"

for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2 ~ /^uperf/{print $1}'); do
    case " $BEFORE " in
    *" $p "*) ;;
    *) kill -TERM "$p" 2>/dev/null ;;
    esac
done
sleep 8
for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2 ~ /^uperf/{print $1}'); do
    case " $BEFORE " in
    *" $p "*) ;;
    *) kill -9 "$p" 2>/dev/null ;;
    esac
done
svc power stayon false 2>/dev/null
say "after teardown: [$($PS -A -o PID,NAME 2>/dev/null | awk '$2 ~ /^uperf/{print $1}' | tr '\n' ' ')]"
cat "$OUT"
