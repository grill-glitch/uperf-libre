#!/system/bin/sh
# ⑥ thermal e2e: the daemon reads a real thermal zone and eases PL1 — the frequency
# targets stay on the cluster's own OPP table. Two runs differing only in the
# threshold env show the scale responding to the *same* real temperature.
# Teardown removes only uperf pids that were new; the real CPU is never written (fake root).
T=/data/local/tmp/therm-e2e
OUT=$T/out.log
PS=/system/bin/ps
say() { echo "$@" >> "$OUT"; }

BEFORE=$($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')
rm -rf "$T"; mkdir -p "$T/user" "$T/root"; : > "$OUT"
say "installed uperf before: $BEFORE"

cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
cp /data/local/tmp/uperf_therm "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! copy failed"

# the fake cpufreq tree the takeover arms in
for p in policy0 policy4 policy7; do
    mkdir -p "$T/root/sys/devices/system/cpu/cpufreq/$p"
    printf 'schedutil\n' >"$T/root/sys/devices/system/cpu/cpufreq/$p/scaling_governor"
    : >"$T/root/sys/devices/system/cpu/cpufreq/$p/scaling_setspeed"
    printf '9999999\n' >"$T/root/sys/devices/system/cpu/cpufreq/$p/scaling_max_freq"
done
say "real zone temps: $(for z in /sys/class/thermal/thermal_zone*; do cat "$z/type" 2>/dev/null | grep -q '^cpu-' && printf '%s=%s ' "$(cat $z/type)" "$(cat $z/temp)"; done | head -c 200)"

run() {
    thresh="$1"
    tag="$2"
    UPERF_FAKE_ROOT="$T/root" \
    UPERF_CPU_GOVERNOR=1 \
    UPERF_SCHED_DRY_RUN=1 \
    UPERF_STATE_FILE="$T/user/orig_governor.txt" \
    UPERF_STATUS_FILE="$T/user/uperf.state" \
    UPERF_STATUS_CONFIG="$T/user/uperf.json" \
    UPERF_THERMAL_THRESH_C="$thresh" \
        "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/log_$tag.txt" </dev/null >/dev/null 2>&1 &
    sleep 8
    say "--- $tag (thresh=$thresh) ---"
    grep -E "Rust: thermal|governor armed" "$T/user/log_$tag.txt" 2>/dev/null | head -4 >>"$OUT"
    # a couple of real tick lines: the frequencies must be OPPs, not temperature-derived
    grep -E "cpu tick|freq" "$T/user/log_$tag.txt" 2>/dev/null | head -2 >>"$OUT"
    for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
        case " $BEFORE " in
        *" $p "*) ;;
        *) kill -TERM "$p" 2>/dev/null ;;
        esac
    done
    sleep 2
}

run 25 cool        # 25 C threshold -> the ~31 C device is "hot"
run 90 cool2       # 90 C threshold -> the same device is "cool"

say "--- real governors (must be untouched) ---"
for d in /sys/devices/system/cpu/cpufreq/policy0 /sys/devices/system/cpu/cpufreq/policy7; do
    say "$d: $(cat "$d/scaling_governor" 2>/dev/null)"
done
say "after teardown: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"
cat "$OUT"
