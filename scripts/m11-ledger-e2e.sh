#!/system/bin/sh
# M11 — the write ledger, end to end, on a device, with the real binary.
#
# The property being proved: every sysfs knob the daemon changes has the value it
# replaced recorded *before* the change, and the shell-side restore (the same function
# the watchdog's dead-man path calls) puts them back — or explicitly reports the ones it
# cannot, never inventing one.
#
# Nothing real is touched: the daemon runs under `UPERF_FAKE_ROOT` (the seam it already
# has for offline runs), the ledger is redirected with `UPERF_SYSFS_ORIG`, and the
# restore is given the matching `UPERF_SYSFS_ROOT` prefix. The real cpufreq tree is
# compared before and after as the control.
#
# Usage:  su -c 'sh /data/local/tmp/m11/m11-ledger-e2e.sh'
# Result: writes <T>/m11.out and <T>/.completed (a truncated log cannot pass for a
#         finished run), and prints a pass/fail count.

set -u
MOD=/data/adb/modules/uperf
T=/data/local/tmp/m11
ROOT="$T/root"
SENTINEL=4242

mkdir -p "$T/user" "$ROOT"
rm -rf "$T"/root/* "$T"/user/* 2>/dev/null

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "    ok   $1"; }
bad() { FAIL=$((FAIL + 1)); echo "    FAIL $1"; }
eq() { # eq <actual> <expected> <what>
    if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (got '$1', want '$2')"; fi
}
contains() { # contains <file> <pattern> <what>
    if [ -f "$1" ] && grep -q "$2" "$1" 2>/dev/null; then ok "$3"; else
        bad "$3 (no match for '$2' in $(basename "$1"))"
        [ -f "$1" ] && sed 's/^/        | /' "$1" | head -6
    fi
}
missing() { # missing <file> <pattern> <what>
    if [ ! -f "$1" ] || ! grep -q "$2" "$1" 2>/dev/null; then ok "$3"; else bad "$3 (unexpected '$2')"; fi
}

echo "== M11 ledger e2e: alioth, kernel $(uname -r), Android $(getprop ro.build.version.release), $(getenforce)"
echo "   module binary: $(stat -c %s "$MOD/bin/uperf" 2>/dev/null) bytes, md5 $(md5sum "$MOD/bin/uperf" 2>/dev/null | cut -c1-12)"

# ---------------------------------------------------------------- fixtures

CFG_SRC=/sdcard/Android/yc/uperf/uperf.json
if [ ! -f "$CFG_SRC" ]; then
    echo " !! no config at $CFG_SRC"
    echo "== M11 ledger e2e: 0 passed, 1 failed (no config)"
    touch "$T/.completed"
    exit 1
fi
# The shipped config disables the sysfs module (`"sysfs": { "enable": false }`), and a
# disabled module plans nothing — so the fixture enables it in its *own* copy. That is
# itself a finding worth stating: on this device the knob table is a config away from
# being live, which is exactly why the ledger has to exist before it is switched on.
awk 'BEGIN{sys=0;done=0}
     /"sysfs"/ {sys=1}
     sys && !done && /"enable"[ ]*:[ ]*false/ {sub(/false/, "true"); done=1}
     {print}' "$CFG_SRC" >"$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || cp -f "$CFG_SRC" "$T/user/uperf.json"
if grep -A 3 '"sysfs"' "$T/user/uperf.json" | grep -q '"enable": *true'; then
    ok "the fixture enabled the sysfs module in its own config copy"
else
    bad "could not enable the sysfs module in the fixture config"
fi
# The binary under test: pushed by the host into $T (the module's installed copy is
# whatever was installed last, which is not necessarily the build being verified).
BIN="$T/bin/uperf"
if [ ! -f "$BIN" ]; then
    BIN="$MOD/bin/uperf"
    echo "   note: no pushed binary at $T/bin/uperf, falling back to the installed one"
fi
cp -f "$BIN" "$T/uperf_test" && chmod 755 "$T/uperf_test"
# The shell side under test, same rule (`uperf_restore_sysfs` is a new function, so an
# old installed copy simply would not have it).
SCRIPTS="$T/script"
[ -f "$SCRIPTS/libuperf.sh" ] || SCRIPTS="$MOD/script"
echo "   binary: $BIN ($(stat -c %s "$BIN" 2>/dev/null) bytes, md5 $(md5sum "$BIN" | cut -c1-12)), scripts: $SCRIPTS"

# Every absolute path the config names is pre-created under the fake root with a value
# we can recognise. Picking the paths *out of the config* (rather than hardcoding a
# knob name) is what makes this a test of the real knob table.
# Only real kernel paths: the config also carries template strings (`"/HOME_PACKAGE/"`)
# that are not paths at all.
grep -o '"/[^"]*"' "$CFG_SRC" 2>/dev/null | tr -d '"' | grep -E '^/(sys|dev|proc)/' | sort -u >"$T/paths.txt"
N_PATHS=$(wc -l <"$T/paths.txt")
while IFS= read -r p; do
    [ -n "$p" ] || continue
    mkdir -p "$ROOT$(dirname "$p")" 2>/dev/null
    printf '%s\n' "$SENTINEL" >"$ROOT$p" 2>/dev/null
done <"$T/paths.txt"
[ "$N_PATHS" -gt 0 ] && ok "the config names $N_PATHS absolute paths, all pre-seeded under the fake root" \
    || bad "the config names no absolute paths (nothing to test)"
# The cpufreq tree is what the takeover needs, and it is NOT in the config's knob table
# (that table holds the scene knobs; the governor path is the daemon's own). It is seeded
# the same way the M9 harness seeds it — and the governor must be a *real* governor: a
# sentinel there is not an error the daemon should paper over, it simply refuses to arm
# (which is what the first run of this script did: the test was wrong, not the daemon).
FAKE_FREQ="$ROOT/sys/devices/system/cpu/cpufreq"
for p in policy0 policy4 policy7; do
    mkdir -p "$FAKE_FREQ/$p"
    printf 'schedutil\n' >"$FAKE_FREQ/$p/scaling_governor"
    : >"$FAKE_FREQ/$p/scaling_setspeed"
    printf '%s\n' "$SENTINEL" >"$FAKE_FREQ/$p/scaling_max_freq"
done
eq "$(cat "$ROOT/sys/devices/system/cpu/cpufreq/policy0/scaling_governor" 2>/dev/null)" "schedutil" \
    "the governor the takeover replaces is a real one (a sentinel would make it refuse to arm)"

# The control: the real tree, byte for byte, before anything runs.
for d in /sys/devices/system/cpu/cpufreq/policy0 /sys/devices/system/cpu/cpufreq/policy7; do
    cat "$d/scaling_governor" 2>/dev/null
done >"$T/real_governors.before"

# ---------------------------------------------------------------- the daemon

UPERF_FAKE_ROOT="$ROOT" \
    UPERF_CPU_GOVERNOR=1 \
    UPERF_SCHED_DRY_RUN=1 \
    UPERF_LOG_MAX_BYTES=4194304 \
    UPERF_STATE_FILE="$T/user/orig_governor.txt" \
    UPERF_STATUS_FILE="$T/user/uperf.state" \
    UPERF_STATUS_CONFIG="$T/user/uperf.json" \
    "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_log.txt" </dev/null >/dev/null 2>&1 &

i=0
ARMED=1
while [ "$i" -lt 60 ]; do
    [ "$(cat "$ROOT/sys/devices/system/cpu/cpufreq/policy0/scaling_governor" 2>/dev/null)" = "userspace" ] && {
        ARMED=0
        break
    }
    sleep 1
    i=$((i + 1))
done
[ "$ARMED" = "0" ] && ok "the test daemon armed the fake policy0 (so the run is a real one)" \
    || { bad "the test daemon never armed"; tail -6 "$T/user/daemon_log.txt" 2>/dev/null | sed 's/^/        | /'; }

# Some knob writes only happen on a mode change; nudge one so the scene path runs.
printf 'powersave' >"$T/user/cur_powermode.txt"
sleep 6

LEDGER="$T/user/sysfs_orig.txt"
if [ -f "$LEDGER" ]; then
    ok "the daemon wrote a ledger next to its status file"
else
    bad "no ledger at $LEDGER"
    tail -10 "$T/user/daemon_log.txt" 2>/dev/null | sed 's/^/        | /'
fi

# Which knobs did the daemon actually change under the fake root?
CHANGED=0
: >"$T/changed.txt"
while IFS= read -r p; do
    [ -n "$p" ] || continue
    v="$(cat "$ROOT$p" 2>/dev/null)"
    [ -n "$v" ] && [ "$v" != "$SENTINEL" ] && {
        echo "$p $v" >>"$T/changed.txt"
        CHANGED=$((CHANGED + 1))
    }
done <"$T/paths.txt"
[ "$CHANGED" -gt 0 ] && ok "the daemon changed $CHANGED fake knob(s): $(head -3 "$T/changed.txt" | tr '\n' ' ')" \
    || bad "the daemon changed no knob under the fake root (nothing for the ledger to restore)"
contains "$LEDGER" "$SENTINEL" "the ledger holds the value that was replaced"
contains "$LEDGER" "Scaling governor|scaling_governor|sys/" "the ledger names real paths"

# ---------------------------------------------------------------- the restore

# The control before the restore: nothing outside the fake root may move.
for d in /sys/devices/system/cpu/cpufreq/policy0 /sys/devices/system/cpu/cpufreq/policy7; do
    cat "$d/scaling_governor" 2>/dev/null
done >"$T/real_governors.after_daemon"
if cmp -s "$T/real_governors.before" "$T/real_governors.after_daemon"; then
    ok "the real governors are untouched by the fake-root run"
else
    bad "the real governors moved: $(cat "$T/real_governors.before" | tr '\n' ' ') -> $(cat "$T/real_governors.after_daemon" | tr '\n' ' ')"
fi

UPERF_SYSFS_ORIG="$LEDGER" UPERF_SYSFS_ROOT="$ROOT" \
    sh -c "cd $SCRIPTS && . ./libuperf.sh && uperf_restore_sysfs" >"$T/restore.out" 2>&1
contains "$T/restore.out" "^uperf: restored [1-9][0-9]* sysfs knob" "the restore reports what it put back"

LEFT=0
while IFS= read -r p; do
    [ -n "$p" ] || continue
    v="$(cat "$ROOT$p" 2>/dev/null)"
    [ -n "$v" ] && [ "$v" != "$SENTINEL" ] && LEFT=$((LEFT + 1))
done <"$T/paths.txt"
eq "$LEFT" "0" "every knob the daemon changed is back to its original value"

if [ -f "$LEDGER" ]; then
    # Only owed entries keep it: everything restored means it must be gone.
    if grep -q '^/' "$LEDGER" 2>/dev/null && ! grep -q "$SENTINEL" "$LEDGER" 2>/dev/null; then
        ok "a kept ledger holds only what is still owed"
    else
        bad "the ledger survived a restore that owed nothing"
        sed 's/^/        | /' "$LEDGER" | head -6
    fi
else
    ok "the ledger is cleared once nothing is owed"
fi
missing "$T/restore.out" "no recorded original for /sys/devices/system/cpu/cpufreq" "no invented value for an unrecorded path"

for d in /sys/devices/system/cpu/cpufreq/policy0 /sys/devices/system/cpu/cpufreq/policy7; do
    cat "$d/scaling_governor" 2>/dev/null
done >"$T/real_governors.after_restore"
if cmp -s "$T/real_governors.before" "$T/real_governors.after_restore"; then
    ok "the real governors are byte-identical to before the whole run"
else
    bad "the real governors changed"
fi

# ---------------------------------------------------------------- teardown

killall uperf_test 2>/dev/null
sleep 1
echo
echo "== M11 ledger e2e: $PASS passed, $FAIL failed"
echo "   artifacts: $T (paths.txt, changed.txt, restore.out, user/sysfs_orig.txt, user/daemon_log.txt)"
touch "$T/.completed"
[ "$FAIL" -eq 0 ]
