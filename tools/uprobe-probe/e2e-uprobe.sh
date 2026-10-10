#!/system/bin/sh
# eBPF/uprobe leg, primitive check: register a tracefs uprobe on libgui.so's
# Surface::queueBuffer and count it per-PID with perf_event_open, so only the target
# app's frames are counted (AppOpt's "避免把其它应用的帧算进去").
#
# Needs: /data/local/tmp/uprobe-count, and libgui64.so's Surface::queueBuffer offset.
T=/sys/kernel/tracing
OFF=${1:-0xf43ec}
PS=/system/bin/ps

echo "=== register the uprobe ==="
echo > $T/uprobe_events
echo "p:uperf_qb /system/lib64/libgui.so:$OFF" > $T/uprobe_events || { echo "!! uprobe register failed"; exit 1; }
cat $T/uprobe_events
ID=$(cat $T/events/uprobes/uperf_qb/id 2>/dev/null)
echo "tracepoint id = $ID"
[ -z "$ID" ] && { echo "!! no id"; exit 1; }

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1
sleep 3
SF=$($PS -A -o PID,NAME 2>/dev/null | awk '/com\.android\.set/{print $1; exit}')
LP=$($PS -A -o PID,NAME 2>/dev/null | awk '/com\.android\.lau/{print $1; exit}')
echo "settings pid=$SF  launcher pid=$LP"

echo "=== count both pids while the settings screen animates ==="
/data/local/tmp/uprobe-count "$ID" "$SF" 5 >/data/local/tmp/up_sf.txt 2>&1 &
A=$!
/data/local/tmp/uprobe-count "$ID" "$LP" 5 >/data/local/tmp/up_lp.txt 2>&1 &
B=$!
sleep 1
i=0
while [ $i -lt 8 ]; do
    input swipe 540 1700 540 500 150 2>/dev/null
    sleep 0.5
    i=$((i + 1))
done
wait $A
wait $B
cat /data/local/tmp/up_sf.txt
cat /data/local/tmp/up_lp.txt

echo "=== control: all pids (global) for 2 s ==="
/data/local/tmp/uprobe-count "$ID" 0 2 2>&1

echo "=== cleanup ==="
echo > $T/uprobe_events
echo "uprobe_events now: [$(cat $T/uprobe_events)]"
