#!/system/bin/sh
# ④ step 1b: flakiness check — does a 2-class dex ever get killed on this boot?
# Run V2 ten times, record every rc, and snapshot dmesg at the end.
LOG=/data/local/tmp/v2loop.log
: > "$LOG"
i=0
fails=0
while [ $i -lt 10 ]; do
  CLASSPATH=/data/local/tmp/V2.jar app_process /system/bin V2 >/dev/null 2>"/data/local/tmp/v2_$i.err"
  rc=$?
  [ "$rc" -ne 0 ] && fails=$((fails+1))
  echo "run$i rc=$rc" >> "$LOG"
  i=$((i+1))
done
echo "fails=$fails/10" >> "$LOG"
echo "--- any err ---" >> "$LOG"
for f in /data/local/tmp/v2_*.err; do cat "$f" >> "$LOG"; done
echo "--- dmesg (oom/kill/dcache) ---" >> "$LOG"
dmesg 2>/dev/null | grep -iE "oom|kill|dcache|vfs|panic|binder" | tail -20 >> "$LOG"
cat "$LOG"
