#!/system/bin/sh
# ============================================================================
# eBPF / uprobe frame-source capability probe  (the ⑤-main-leg gate)
#
# *** NOT YET RUN ***  — written while the device was off USB (see the commit
# message and docs/m11-ebpf-frame-source.md §0). Every line below is the check
# I would run, not a result I have. Do not read anything in this file as
# evidence until it has produced output on a real device.
#
# What it answers, in gate order:
#   1. does the kernel have BPF / uprobes at all?
#   2. is tracefs reachable (which path)?
#   3. can this domain *write* uprobe_events / dynamic_events?  (SELinux!)
#   4. which queueBuffer-ish symbol does libgui.so actually export?
#   5. does a real uprobe on it attach, and FIRE for a live foreground app?
#
# Why uprobes and not literally a BPF program: an uprobe on `libgui.so`'s
# queueBuffer is what AppOpt's eBPF program attaches to; the kernel-side
# mechanism is the same. If tracefs works here, the frame leg needs no aya, no
# `-target bpf` build step and no BPF program at all — it needs a per-pid event
# count. That is a deviation worth taking deliberately, and worth writing down.
# Step 6 covers the literal-BPF question if we ever need it.
#
# Run as root:   su -c 'sh /data/local/tmp/ebpf-probe.sh'
# ============================================================================
E=""
for c in /sys/kernel/tracing /sys/kernel/debug/tracing; do
    [ -d "$c" ] && E="$c" && break
done
echo "== 0. environment =="
echo "kernel   : $(uname -r)"
echo "selinux  : $(getenforce 2>/dev/null)"
echo "tracefs  : ${E:-<not mounted/visible>}"

echo
echo "== 1. kernel config (BPF / uprobes) =="
if [ -r /proc/config.gz ]; then
    zcat /proc/config.gz 2>/dev/null | grep -E "CONFIG_(BPF|BPF_SYSCALL|UPROBE|KPROBE|KPROBE_EVENTS|PERF_EVENTS|HAVE_BPF|DEBUG_FS)" | sort
else
    echo "/proc/config.gz not readable -> fall back to the interface checks below"
fi
echo "--- bpf-related sysctls ---"
for f in /proc/sys/kernel/unprivileged_bpf_disabled /proc/sys/kernel/perf_event_paranoid; do
    [ -r "$f" ] && echo "$f = $(cat "$f")"
done
echo "--- is there a bpf fs? ---"
ls -d /sys/fs/bpf 2>&1

echo
echo "== 2. tracefs interfaces =="
if [ -n "$E" ]; then
    for f in uprobe_events dynamic_events kprobe_events tracing_on trace trace_pipe; do
        printf '%-16s ' "$f"
        ls -la "$E/$f" 2>&1 | awk '{print $1, $3, $4, $NF}'
    done
    echo "--- events/uprobes ---"
    ls "$E/events/uprobes" 2>&1 | head -5
else
    echo "no tracefs -> the uprobe route is unavailable; try mounting debugfs:"
    echo "  mount -t tracefs tracefs /sys/kernel/tracing"
    mount -t tracefs tracefs /sys/kernel/tracing 2>&1 && E=/sys/kernel/tracing && echo "mounted ok: $E"
fi

echo
echo "== 3. write access (this is where SELinux usually says no) =="
if [ -n "$E" ]; then
    # a no-op rewrite: report the exact errno, not a guess
    ( : > "$E/uprobe_events" ) 2>&1 && echo "uprobe_events: writable" || echo "uprobe_events: NOT writable (see errno above)"
    ( : > "$E/dynamic_events" ) 2>&1 && echo "dynamic_events: writable" || echo "dynamic_events: NOT writable (see errno above)"
fi

echo
echo "== 4. libgui.so and its queueBuffer-ish symbols =="
for so in /system/lib64/libgui.so /system/lib/libgui.so /apex/com.android.graphics.*/lib64/libgui.so; do
    [ -f "$so" ] || continue
    echo "--- $so ($(stat -c %s "$so" 2>/dev/null) bytes)"
    for sym in queueBuffer queueBufferInternal queueBufferAsync queueBufferImpl; do
        n=$(grep -a -c "$sym" "$so" 2>/dev/null)
        echo "    string '$sym': $n hit(s)"
    done
    # the dynamic symbol table is what the kernel's uprobe resolver reads
    if command -v nm >/dev/null 2>&1; then
        nm -D "$so" 2>/dev/null | grep -i queuebuffer | head -5
    fi
done

echo
echo "== 5. the real test: attach a uprobe and see if it fires =="
if [ -z "$E" ]; then
    echo "no tracefs -> skipped"
else
    SO=/system/lib64/libgui.so
    [ -f "$SO" ] || SO=/system/lib/libgui.so
    echo "target: $SO"
    ( : > "$E/uprobe_events" ) 2>/dev/null
    for sym in queueBuffer queueBufferInternal queueBufferAsync; do
        line="p:uperf_probe_$sym $SO:$sym"
        echo "trying: $line"
        if echo "$line" >>"$E/uprobe_events" 2>&1; then
            echo "  -> attached"
            EV="$E/events/uprobes/uperf_probe_$sym"
            [ -d "$EV" ] && { echo 1 > "$EV/enable" 2>&1; echo "  enabled: $([ -r "$EV/enable" ] && cat "$EV/enable")"; }
        else
            echo "  -> rejected (symbol missing, or no permission)"
        fi
    done
    echo "--- registered probes ---"
    cat "$E/uprobe_events" 2>/dev/null

    echo "--- generate frames: launch an app and swipe for ~6 s ---"
    echo 1 > "$E/tracing_on" 2>/dev/null
    ( : > "$E/trace" ) 2>/dev/null
    am start -a android.settings.SETTINGS >/dev/null 2>&1
    i=0
    while [ $i -lt 6 ]; do
        input swipe 540 1700 540 500 150 2>/dev/null
        sleep 1
        i=$((i + 1))
    done
    echo "--- trace: uprobe hits (first 15) ---"
    grep -c "uperf_probe" "$E/trace" 2>/dev/null | sed 's/^/hits: /'
    grep "uperf_probe" "$E/trace" 2>/dev/null | head -15
    echo "--- per-pid hit counts (what an FPS reader would use) ---"
    grep "uperf_probe" "$E/trace" 2>/dev/null | awk '{print $3}' | sort | uniq -c | sort -rn | head -8

    echo "--- cleanup ---"
    for sym in queueBuffer queueBufferInternal queueBufferAsync; do
        [ -d "$E/events/uprobes/uperf_probe_$sym" ] && echo 0 > "$E/events/uprobes/uperf_probe_$sym/enable" 2>/dev/null
    done
    ( : > "$E/uprobe_events" ) 2>/dev/null
    echo "probes left: $(wc -l < "$E/uprobe_events" 2>/dev/null)"
fi

echo
echo "== 6. (only if 1-5 pass) the literal-BPF question =="
echo "A BPF program would need bpf(2) + a verifier-permitted loader in THIS domain."
echo "Not testable from a shell script; if we ever want it, it needs a tiny"
echo "cross-compiled prober that calls bpf(BPF_MAP_CREATE) and reports errno."
echo "Note the uprobe route above needs none of that."
