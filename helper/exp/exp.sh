#!/system/bin/sh
# ④ diagnosis step 1: is the multi-class dex killed by the kernel? Capture rc, timing,
# stderr/stdout of each variant and dmesg around it, in ONE window.
LOG=/data/local/tmp/exp.log
: > "$LOG"
say() { echo "$@" >> "$LOG"; }
run() {
  n="$1"
  u0=$(cut -d' ' -f1 /proc/uptime)
  say "=== $n uptime0=$u0 ==="
  CLASSPATH="/data/local/tmp/$n.jar" app_process /system/bin "$n" \
      >"/data/local/tmp/$n.stdout" 2>"/data/local/tmp/$n.stderr"
  rc=$?
  u1=$(cut -d' ' -f1 /proc/uptime)
  say "$n rc=$rc uptime1=$u1"
  say "--- stdout ---"; cat "/data/local/tmp/$n.stdout" >> "$LOG" 2>/dev/null
  say "--- stderr ---"; cat "/data/local/tmp/$n.stderr" >> "$LOG" 2>/dev/null
  say "--- dmesg tail ---"; dmesg 2>/dev/null | tail -15 >> "$LOG"
}
say "sdk=$(getprop ro.build.version.sdk) rel=$(getprop ro.build.version.release) model=$(getprop ro.product.model)"
say "sep=$(getenforce) uid=$(id)"
run V1
run V2
run V2
run V1big
run V3
say "=== logcat oom/lowmem/kill (last 60) ==="
logcat -d -b all 2>/dev/null | grep -iE "oom|lowmemory|kill|V2|V3|app_process" | tail -60 >> "$LOG"
say "=== end ==="
cat "$LOG"
