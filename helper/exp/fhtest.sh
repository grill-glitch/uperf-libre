#!/system/bin/sh
# ④ step 3: does the real helper (2 classes: ForegroundHelper + inner StackListener)
# run, and does getFocusedRootTaskInfo() report the true top package across am start?
OUT=/data/local/tmp/foreground.txt
LOG=/data/local/tmp/fh.log
: > "$LOG"
rm -f "$OUT" "$OUT.new"
CLASSPATH=/data/local/tmp/foreground.jar app_process /system/bin ForegroundHelper "$OUT" 1200 \
   </dev/null >/dev/null 2>/data/local/tmp/fh.stderr &
HPID=$!
sleep 3
echo "helper pid=$HPID alive_now=$(kill -0 $HPID 2>/dev/null && echo yes || echo no)" >> "$LOG"
echo "[t0] $(cat $OUT 2>/dev/null)" >> "$LOG"
input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
input keyevent KEYCODE_HOME 2>/dev/null; sleep 3
echo "[home] $(cat $OUT 2>/dev/null)" >> "$LOG"
am start -a android.settings.SETTINGS >/dev/null 2>&1; sleep 3
echo "[settings] $(cat $OUT 2>/dev/null)" >> "$LOG"
input keyevent KEYCODE_HOME 2>/dev/null; sleep 3
echo "[home2] $(cat $OUT 2>/dev/null)" >> "$LOG"
am start -n com.android.documentsui/.files.FilesActivity >/dev/null 2>&1; sleep 3
echo "[files] $(cat $OUT 2>/dev/null)" >> "$LOG"
echo "helper_still_alive=$(kill -0 $HPID 2>/dev/null && echo yes || echo no)" >> "$LOG"
kill "$HPID" 2>/dev/null
sleep 1
echo "after_kill_alive=$(kill -0 $HPID 2>/dev/null && echo yes || echo no)" >> "$LOG"
echo "--- helper stderr (tail) ---" >> "$LOG"
tail -20 /data/local/tmp/fh.stderr >> "$LOG" 2>/dev/null
cat "$LOG"
