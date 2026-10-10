#!/system/bin/sh
# ⑪ e2e against a source that works even while the device is locked: the lockscreen shade
# (SystemUI's RenderThread) animates through the same libgui path. Pins the target pid, so
# this measures ⑪'s counting, not ④'s foreground reader. An independent PMU reading of the
# same RenderThread over the same period is the control.
T=/data/local/tmp/up-shade
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

SU=$($PS -A -o PID,NAME 2>/dev/null | awk '$2 == "com.android.systemui" {print $1; exit}')
RT=$(for t in /proc/$SU/task/*; do grep -q RenderThread $t/comm 2>/dev/null && basename $t; done | head -1)
say "target systemui pid=$SU RenderThread=$RT"

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_TICK_MS=1000 \
UPERF_SF_BINDER_WINDOW_MS=1000 \
UPERF_SF_UPROBE=1 \
UPERF_SF_UPROBE_PID="$SU" \
UPERF_SF_UPROBE_WINDOW_MS=1000 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$LOG" </dev/null >/dev/null 2>&1 &

n=0
while [ $n -lt 20 ]; do
    grep -q 'sf-uprobe attached' "$LOG" 2>/dev/null && break
    sleep 1
    n=$((n + 1))
done
say "attach      : $(grep -E 'sf-uprobe attached' "$LOG" | head -1)"

# toggle the shade: expand/collapse animates the lockscreen, queueing real frames
(
    i=0
    while [ $i -lt 40 ]; do
        if [ $((i % 2)) -eq 0 ]; then cmd statusbar expand-notifications 2>/dev/null
        else cmd statusbar collapse 2>/dev/null; fi
        sleep 0.7
        i=$((i + 1))
    done
) >/dev/null 2>&1 &
SW=$!
sleep 6
say "independent : $(timeout 20 /data/local/tmp/uprobe-count pmu /system/lib64/libgui.so 0xf43ec $RT 5 2>&1)"
sleep 3
kill $SW 2>/dev/null

say "daemon fps  : $(grep -E 'sf-uprobe .*fps=' "$LOG" | tail -10 | tr '\n' '|')"
say "frames.state: $(tr '\n' ' ' <"$T/user/uperf_frames.state" 2>/dev/null)"
say "src changes : $(grep -E 'frame source ->' "$LOG" | tail -3 | tr '\n' '|')"

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
say "after teardown: [$($PS -A -o PID,NAME 2>/dev/null | awk '$2 ~ /^uperf/{print $1}' | tr '\n' ' ')]"
cat "$OUT"
