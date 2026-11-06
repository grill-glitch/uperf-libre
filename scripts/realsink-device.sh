#!/system/bin/sh
# RealSink on the device: the executor that writes live nodes, exercised without touching
# kernel state. The knob table points at ordinary files under /data/local/tmp, so every
# path in the code is the real one (`modules.sysfs.enable: true` -> `RealSink`), but the
# bytes land somewhere harmless.
#
# Checks: the write lands, the ledger records the original *before* the first write, a
# repeated identical value is skipped (dedup), a node that ignores the write is reported
# as ineffective (/dev/null), and with `enable: false` nothing is written at all.
set -u
T=/data/local/tmp/realsink
BIN=$T/uperf
LOG=$T/daemon.log
FAIL=0
bad() { echo "FAIL: $*"; FAIL=$((FAIL + 1)); }
note() { echo "$*"; }

rm -rf $T
mkdir -p $T/knobs
cp -f /data/local/tmp/uperf_real $BIN || { echo "FAIL: binary missing"; exit 1; }
chmod 755 $BIN
printf '0-7\n' > $T/knobs/cpus
printf '0-6\n' > $T/knobs/cpus_re
printf '' > $T/knobs/procs

mkcfg() {  # $1 = enable (true/false), $2 = path
    cat > "$2" <<EOF
{
  "meta": {"name": "realsink", "author": "audit"},
  "modules": {
    "switcher": {"hintDuration": {"idle": 0.0, "touch": 4.0, "trigger": 0.03,
                                   "gesture": 0.1, "switch": 0.4, "junk": 0.06},
                 "switchInode": "$T/cur_powermode.txt"},
    "sysfs": {"enable": $1, "knob": {
        "cpusetTa": "$T/knobs/cpus",
        "cpusetRe": "$T/knobs/cpus_re",
        "cgroupTa": "$T/knobs/procs",
        "blackhole": "/dev/null"
    }}
  },
  "initials": {"sysfs": {"cpusetTa": "0-7", "cpusetRe": "0-6", "cgroupTa": "", "blackhole": "1"}},
  "presets": {
    "balance": {"*": {}},
    "perf":    {"*": {"sysfs.cpusetTa": "0-3", "sysfs.cpusetRe": "0-2",
                      "sysfs.cgroupTa": "101,102,103", "sysfs.blackhole": "2"}},
    "perf2":   {"*": {"sysfs.cpusetTa": "0-3", "sysfs.cpusetRe": "0-2",
                      "sysfs.cgroupTa": "101,102,103", "sysfs.blackhole": "2"}}
  }
}
EOF
}

run_case() {  # $1 = enable, $2 = tag
    rm -f $T/daemon.log $T/cur_powermode.txt $T/sysfs_orig.txt
    printf 'balance' > $T/cur_powermode.txt
    printf '0-7\n' > $T/knobs/cpus
    printf '0-6\n' > $T/knobs/cpus_re
    : > $T/knobs/procs
    mkcfg "$1" $T/uperf.json
    cd $T
    UPERF_LOG_MAX_BYTES=262144 UPERF_SYSFS_ORIG=$T/sysfs_orig.txt \
      $BIN -o $LOG $T/uperf.json >/dev/null 2>&1
    sleep 3
    note "case $2: preset -> perf"
    printf 'perf' > $T/cur_powermode.txt
    sleep 3
    note "  cpus=$(cat $T/knobs/cpus | tr '\n' '|') re=$(cat $T/knobs/cpus_re | tr '\n' '|')"
    note "  procs=$(cat $T/knobs/procs | tr '\n' '|')"
    note "  ledger=$([ -f $T/sysfs_orig.txt ] && tr '\n' ';' < $T/sysfs_orig.txt || echo '(none)')"
    case "$2" in
    on)
        [ "$(cat $T/knobs/cpus)" = "0-3" ] || bad "cpus not written (got '$(cat $T/knobs/cpus)')"
        [ "$(cat $T/knobs/cpus_re)" = "0-2" ] || bad "cpus_re not written"
        [ "$(tr '\n' ',' < $T/knobs/procs)" = "101,102,103," ] || bad "pid list not written one per line: '$(cat $T/knobs/procs | tr '\n' ',')'"
        grep -q "cpus 0-7" $T/sysfs_orig.txt 2>/dev/null || bad "ledger has no original for cpus: $(cat $T/sysfs_orig.txt 2>/dev/null)"
        grep -qE "sysfs real-write [1-9][0-9]* ok" $LOG || bad "no real-write summary with writes in the log"
        # dedup: a *different* preset that resolves to the same values. The plan is
        # produced (a mode change is real work) but every value is already in effect, so
        # the sink must skip all of them — that is upstream's diff rule. Re-selecting the
        # same preset would prove nothing: the watcher dedups the mode string first.
        before_skip=$(grep -oE "skipped=[0-9]+" $LOG | tail -1 | cut -d= -f2)
        printf 'perf2' > $T/cur_powermode.txt
        sleep 3
        after_skip=$(grep -oE "skipped=[0-9]+" $LOG | tail -1 | cut -d= -f2)
        note "  dedup: skipped $before_skip -> $after_skip"
        [ "${after_skip:-0}" -gt "${before_skip:-0}" ] || bad "a repeated identical switch was not deduped"
        grep -q "read back as" $LOG || bad "/dev/null write was not reported as ineffective"
        grep -q "would-write" $LOG && bad "the enabled case logged would-write"
        ;;
    off)
        [ "$(cat $T/knobs/cpus)" = "0-7" ] || bad "enable=false still wrote cpus: '$(cat $T/knobs/cpus)'"
        grep -q "would-write $T/knobs/cpus" $LOG || bad "enable=false did not log would-write"
        grep -q "sysfs real-write" $LOG && bad "enable=false used the real sink"
        ;;
    esac
    # Stop whatever we started (this config's daemon), leaving the module's own alone.
    for p in $(ps -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
        case " $BEFORE " in
        *" $p "*) ;;
        *) kill -TERM "$p" 2>/dev/null ;;
        esac
    done
    sleep 2
}

BEFORE=$(ps -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')
run_case true on
run_case false off
note "== RESULT: $FAIL failure(s) =="
[ "$FAIL" = "0" ] || exit 1
