#!/system/bin/sh
# Does per-PID perf counting of the uprobe track real frames? One run, with the trace
# buffer enabled as ground truth over the same window and a swipe generator driving the
# target app. Every child is bounded by `timeout` so nothing can wedge the shell.
T=/sys/kernel/tracing
PS=/system/bin/ps
UC=/data/local/tmp/uprobe-count
OFF=${1:-0xf43ec}

echo > $T/uprobe_events 2>/dev/null
echo "p:uperf_qb /system/lib64/libgui.so:$OFF" > $T/uprobe_events || { echo "!! register failed"; exit 1; }
ID=$(cat $T/events/uprobes/uperf_qb/id 2>/dev/null)
echo "tracepoint id = $ID"

input keyevent KEYCODE_WAKEUP 2>/dev/null; wm dismiss-keyguard 2>/dev/null
input keyevent KEYCODE_HOME 2>/dev/null; sleep 1
am start -a android.settings.SETTINGS >/dev/null 2>&1
sleep 3
SF=$($PS -A -o PID,NAME 2>/dev/null | awk '/com\.android\.set/{print $1; exit}')
LP=$($PS -A -o PID,NAME 2>/dev/null | awk '/com\.android\.lau/{print $1; exit}')
echo "settings pid=$SF  launcher pid=$LP"

# ground truth: the trace buffer records every hit (any pid) for the same window
OLD=$(cat $T/tracing_on)
echo 1 > $T/tracing_on
echo 1 > $T/events/uprobes/uperf_qb/enable
echo > $T/trace

# drive frames on the settings screen for the whole window
(
    i=0
    while [ $i -lt 14 ]; do
        if [ $((i % 2)) -eq 0 ]; then input swipe 540 1700 540 600 120 2>/dev/null
        else input swipe 540 600 540 1700 120 2>/dev/null; fi
        sleep 0.4
        i=$((i + 1))
    done
) >/dev/null 2>&1 &
SW=$!

echo "--- per-task on the settings pid (4 s) ---"
timeout 15 $UC "$ID" "$SF" 4 2>&1
echo "--- per-task on the launcher pid (4 s, control) ---"
timeout 15 $UC "$ID" "$LP" 4 2>&1
echo "--- per-CPU (4 s) ---"
timeout 15 $UC "$ID" 0 4 2>&1

echo "trace ground truth (any pid, whole window): $(grep -c 'uperf_qb:' $T/trace) hits"
grep 'uperf_qb:' $T/trace | head -2

kill $SW 2>/dev/null
echo 0 > $T/events/uprobes/uperf_qb/enable
echo "$OLD" > $T/tracing_on
echo > $T/uprobe_events
echo "tracing_on restored to $OLD; probe=[$(cat $T/uprobe_events)]"
