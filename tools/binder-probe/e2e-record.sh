#!/system/bin/sh
# ⑧ recording e2e: opt-in per-app sessions, the >=3 min rule (lowered here so a short run
# can produce one), DEFLATE store, dedupe by pkg+epoch. Teardown removes only new pids.
T=/data/local/tmp/rec-e2e
OUT=$T/out.log
PS=/system/bin/ps
say() { echo "$@" >> "$OUT"; }

BEFORE=$($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')
rm -rf "$T"; mkdir -p "$T/user" "$T/root"; : > "$OUT"
say "installed uperf before: $BEFORE"

cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
cp /data/local/tmp/uperf_rec "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! copy failed"

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1

UPERF_FAKE_ROOT="$T/root" \
UPERF_SCHED_DRY_RUN=1 \
UPERF_STATE_FILE="$T/user/orig_governor.txt" \
UPERF_STATUS_FILE="$T/user/uperf.state" \
UPERF_STATUS_CONFIG="$T/user/uperf.json" \
UPERF_RECORD=1 \
UPERF_RECORD_MIN_MS=4000 \
UPERF_RECORD_SAMPLE_MS=1000 \
UPERF_SF_BINDER=1 \
UPERF_SF_BINDER_LAYER=com.android.settings/com.android.settings.Settings#0 \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &
sleep 2

# two foreground stretches, each longer than the (lowered) 4 s rule
am start -a android.settings.SETTINGS >/dev/null 2>&1
sleep 7
am start -n com.android.documentsui/.files.FilesActivity >/dev/null 2>&1
sleep 7

say "--- daemon: recorder lines ---"
grep -E "recorder" "$T/user/daemon_log.txt" 2>/dev/null | head -8 >>"$OUT"

# stop: the open session must be closed and committed on the way out
for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
    case " $BEFORE " in
    *" $p "*) ;;
    *) kill -TERM "$p" 2>/dev/null ;;
    esac
done
sleep 2
say "--- after stop ---"
grep -E "recorder" "$T/user/daemon_log.txt" 2>/dev/null | tail -3 >>"$OUT"
say "--- history dir ---"
ls -la "$T/user/history" 2>/dev/null >>"$OUT"
say "--- file magic (first bytes) ---"
for f in "$T"/user/history/*.dfl; do
    [ -f "$f" ] && say "$(basename "$f"): $(od -An -tx1 -N4 "$f" 2>/dev/null)"
done
say "after teardown: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"
cat "$OUT"
