#!/system/bin/sh
# ⑤ device e2e: the daemon's direct-binder SF frame source (UPERF_SF_BINDER=1)
# sampling `--latency` for a real layer while frames are being produced.
# Teardown removes only uperf processes that were not running before.
T=/data/local/tmp/sfb-e2e
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

# bring an app up and resolve its layer, so the sample has a real target
input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1; sleep 3
say "layer will be resolved by the daemon from the top app (pick_layer)"

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_TICK_MS=1000 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &
sleep 3
say "new uperf: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"

# Generate real frames: alternate launching Settings and going home, so each
# transition animates the settings layer the daemon is measuring.
i=0
while [ $i -lt 9 ]; do
    am start -a android.settings.SETTINGS >/dev/null 2>&1
    sleep 1
    input keyevent KEYCODE_HOME 2>/dev/null
    sleep 1
    i=$((i + 1))
done

say "--- daemon log: sf-binder lines ---"
grep -E "sf-binder|sf_binder" "$T/user/daemon_log.txt" 2>/dev/null | tail -14 >> "$OUT"
say "--- tail ---"
tail -3 "$T/user/daemon_log.txt" 2>/dev/null >> "$OUT"

# teardown: TERM only the uperf pids that are new since we started
for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
    case " $BEFORE " in
    *" $p "*) ;;
    *) kill -TERM "$p" 2>/dev/null ;;
    esac
done
sleep 2
say "after teardown: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"
cat "$OUT"
