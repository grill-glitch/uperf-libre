#!/system/bin/sh
#
# M9 device verification (runs ON the device, via `su -c 'sh ...'`).
#
# Verifies what the host harness cannot: that `/proc/<pid>/exe` survives dfps'
# cmdline rewrite on a real device, that the scan judges the real supervisor /
# worker pair correctly, that a real daemon either disarms on SIGTERM or is saved
# by the script-side restore when it cannot, that the two status files land where
# they are supposed to, and what the watchdog costs.
#
# Driver (host side), from the repo root:
#
#   T=/data/local/tmp/m9
#   adb -s <serial> push scripts/m9-device-verify.sh $T/m9-device-verify.sh
#   adb -s <serial> push magisk/script/uperf_watchdog.sh magisk/script/pathinfo.sh \
#        magisk/script/libcommon.sh magisk/script/libcgroup.sh \
#        magisk/script/libuperf.sh $T/script/
#   adb -s <serial> shell su -c "sh $T/m9-device-verify.sh"
#
# Safety
# ------
# * The real cpufreq tree is never written. The takeover is exercised against a
#   *fake* sysfs root (`UPERF_FAKE_ROOT`) that both the daemon's writes and the
#   watchdog's view point at; the real governors are captured before and after and
#   compared byte for byte.
# * `UPERF_SCHED_DRY_RUN=1`, so the co-processed context scheduler applies
#   nothing (sched_setaffinity has no path to redirect).
# * The installed module's daemon is only *observed*: part 1 runs the watchdog
#   with `UPERF_WATCHDOG_DRY_RUN=1`, so it cannot tear down the module the user is
#   running. Parts 3/4 use a *copy* of the module binary at a different path,
#   because the watchdog identifies processes by image — sharing the installed
#   `exe` would make the scan see two supervisors and (correctly) retire both.

T=/data/local/tmp/m9
MOD=/data/adb/modules/uperf
REAL_EXE="$MOD/bin/uperf"
FAKE_EXE="$T/fake_uperf"
FAKE_ROOT="$T/sys"
FAKE_FREQ="$FAKE_ROOT/sys/devices/system/cpu/cpufreq"
POLICIES="policy0 policy4 policy7"
PS=/system/bin/ps

PASS=0
FAIL=0
ok() {
    PASS=$((PASS + 1))
    echo "    ok   $1"
}
bad() {
    FAIL=$((FAIL + 1))
    echo "    FAIL $1"
}
check() { # check <description> <status: 0 = pass>
    if [ "$2" = "0" ]; then ok "$1"; else bad "$1"; fi
}
eq() { # eq <actual> <expected> <description>
    if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (got '$1', want '$2')"; fi
}
has() { # has <file> <pattern> <description>
    if grep -q "$2" "$1" 2>/dev/null; then
        ok "$3"
    else
        bad "$3 (no match for '$2' in $(basename "$1"))"
        [ -f "$1" ] && tail -20 "$1" | sed 's/^/        | /'
    fi
}
lacks() { # lacks <file> <pattern> <description>
    if [ ! -f "$1" ] || ! grep -q "$2" "$1"; then ok "$3"; else bad "$3 (unexpected '$2')"; fi
}

# Every process whose image is $1. A zombie has no readable `/proc/<pid>/exe`, so
# zombies are counted separately by comm (a zombie keeps its name).
exe_procs() {
    local p pid comm exe
    for p in /proc/[0-9]*; do
        # Same cheap filter the watchdog uses: our processes carry the fixed name
        # dfps sets (`PROC_NAME` = uperf), and a `readlink` per pid costs ~13 ms here
        # — an unfiltered pass over 1289 pids took 40 s and dominated this harness.
        comm=""
        # The group's stderr is redirected, not the command's: the shell prints a
        # failed redirection itself when a pid vanishes mid-loop.
        { IFS= read -r comm <"$p/comm"; } 2>/dev/null
        [ "$comm" = "uperf" ] || continue
        pid="${p##*/}"
        exe="$(readlink "$p/exe" 2>/dev/null)"
        case "$exe" in
        *" (deleted)") exe="${exe% (deleted)}" ;;
        esac
        [ "$exe" = "$1" ] || continue
        echo "$pid"
    done
}
ppid_of() { # pure shell, like the watchdog: a fork is ~10 ms here
    local line
    { IFS= read -r line <"/proc/$1/stat"; } 2>/dev/null || return 1
    # shellcheck disable=SC2086
    set -- ${line##*) }
    echo "$2"
}
ticks_of() { sed -n 's/^.*) //p' "/proc/$1/stat" 2>/dev/null | awk '{print $12 + $13}'; }
uperf_zombies() {
    local n
    n="$($PS -A -o STAT,NAME 2>/dev/null | awk '$2 == "uperf" && $1 ~ /^Z/ { c++ } END { print c + 0 }')"
    case "$n" in
    '' | *[!0-9]*) n=0 ;;
    esac
    echo "$n"
}
# A fake-exe process left over from an aborted run makes the next run's part 1 see a
# stranger (part 1 identifies the installed daemon by image, but the fake one shares
# the name). SIGKILL is fine here: the takeover these processes hold is a fake sysfs
# root, not the real CPU.
cleanup_fakes() {
    local p
    for p in $(exe_procs "$FAKE_EXE"); do
        kill -KILL "$p" 2>/dev/null
    done
    return 0
}

real_governors() {
    for d in /sys/devices/system/cpu/cpufreq/policy*; do
        echo "$(basename "$d")=$(cat "$d/scaling_governor" 2>/dev/null)"
    done | tr '\n' ' '
}

# ---------------------------------------------------------------- setup

trap 'cleanup_fakes' EXIT INT TERM

echo "== M9 device verification: $(getprop ro.product.device), kernel $(uname -r), Android $(getprop ro.build.version.release), $(getenforce)"
REAL_BEFORE="$(real_governors)"
echo "   real governors before: $REAL_BEFORE"

# Only the pieces this run owns are cleared: wiping the whole harness directory
# here deleted the pushed module scripts the run needs (caught on the first device
# run, which then bailed out of its own setup).
mkdir -p "$T/script" "$T/flag" "$T/p1/user" "$T/p1/flag" "$T/p3"
rm -rf "$T/sys" "$T/daemon_log.txt" "$T/orig_governor.txt" "$T/uperf.state"
[ -f "$T/script/uperf_watchdog.sh" ] || { echo " !! push $T/script/uperf_watchdog.sh (+ pathinfo/libcommon/libcgroup/libuperf) first"; exit 1; }
[ -x "$REAL_EXE" ] || { echo " !! $REAL_EXE missing — is the module installed?"; exit 1; }
[ -s "$REAL_EXE" ] || exit 1
# The test daemon must be the *current* build: the installed module still carries
# the previous one, which has neither `status.rs` (the status file) nor the M9
# wiring. Pushed by the driver from `build/.../runnable/uperf`.
[ -s "$FAKE_EXE" ] || { echo " !! push the freshly built binary to $FAKE_EXE first"; exit 1; }
chmod 755 "$FAKE_EXE"
# The test binary must be a *distinct image* (identity is by `exe`, so sharing the
# installed one's would make the scan see two supervisors) and, when the driver says
# which build to expect, exactly that build. Comparing the two files for equality was
# wrong once the installed module became the current build.
if [ -n "$(exe_procs "$REAL_EXE")" ] && [ "$FAKE_EXE" = "$REAL_EXE" ]; then
    echo " !! $FAKE_EXE is the installed image; the test copy must be at another path"
    exit 1
fi
if [ -f "$T/expected_bin_md5" ]; then
    want="$(cat "$T/expected_bin_md5" 2>/dev/null)"
    got="$(md5sum "$FAKE_EXE" 2>/dev/null | cut -d' ' -f1)"
    if [ -n "$want" ] && [ "$got" != "$want" ]; then
        echo " !! $FAKE_EXE is $got but the driver expects $want"
        exit 1
    fi
    echo "   test binary md5 verified against the driver's build ($got)"
fi
cp -f /sdcard/Android/yc/uperf/uperf.json "$T/t3.json" 2>/dev/null || { echo " !! no config at /sdcard/Android/yc/uperf/uperf.json"; exit 1; }
cleanup_fakes
echo "   test binary: $FAKE_EXE ($(stat -c %s "$FAKE_EXE") bytes)"

# ------------------------------------------------- part 1: the real daemon

echo
echo "== part 1: does the scan judge the real daemon correctly? (dry run, read-only)"
REAL_SUP=""
REAL_WORK=""
REAL_BEFORE_PIDS="$(exe_procs "$REAL_EXE" | tr '\n' ' ')"
# By image, not by name: every process of ours is named `uperf` (that is the point
# of the identity rule), so a name filter would also pick up a test copy.
for p in $(exe_procs "$REAL_EXE"); do
    wd_stat_fields "$p" 2>/dev/null || true
    ppid="$(ppid_of "$p")"
    exe="$(readlink "/proc/$p/exe" 2>/dev/null)"
    echo "   pid=$p ppid=$ppid cmdline=[$(tr '\0' ' ' <"/proc/$p/cmdline" 2>/dev/null)] exe=$exe"
    eq "$exe" "$REAL_EXE" "pid $p: exe survives the cmdline rewrite"
    if [ "$ppid" = "1" ]; then REAL_SUP="$p"; else REAL_WORK="$p"; fi
done
[ -n "$REAL_SUP" ] && [ -n "$REAL_WORK" ] && c=0 || c=1
check "the installed daemon is a supervisor ($REAL_SUP) + worker ($REAL_WORK) pair" "$c"

env UPERF_WATCHDOG_EXE="$REAL_EXE" \
    UPERF_WATCHDOG_CPUFREQ_ROOT=/sys/devices/system/cpu/cpufreq \
    UPERF_WATCHDOG_PROC_ROOT=/proc \
    UPERF_WATCHDOG_USER_PATH="$T/p1/user" \
    UPERF_WATCHDOG_FLAG_PATH="$T/p1/flag" \
    UPERF_WATCHDOG_LOG="$T/p1/wd.log" \
    UPERF_WATCHDOG_STATE="$T/p1/wd.state" \
    UPERF_WATCHDOG_DRY_RUN=1 \
    UPERF_WATCHDOG_INTERVAL=2 \
    UPERF_WATCHDOG_GRACE=3 \
    sh "$T/script/uperf_watchdog.sh" >"$T/p1/out.log" 2>&1 &
WD1=$!
sleep 7
kill -TERM "$WD1" 2>/dev/null
wait "$WD1" 2>/dev/null
sleep 1

has "$T/p1/wd.log" "healthy: sup=\[$REAL_SUP\] workers=\[$REAL_WORK\]" "the watchdog calls the real pair healthy, with the real pids"
has "$T/p1/wd.log" "armed=\[\]" "it reads the real cpufreq tree: nothing armed"
lacks "$T/p1/wd.log" "unhealthy" "no false unhealthy sample in 7 s of sampling"
eq "$(sed -n 's/^state=//p' "$T/p1/wd.state" | head -n 1)" "stopped" "the dry run wrote a state file that the stop then closed"
has "$T/p1/wd.state" "last state: running" "the observed state was running"
eq "$(exe_procs "$REAL_EXE" | tr '\n' ' ')" "$REAL_BEFORE_PIDS" "the installed daemon is untouched (same pids)"
eq "$(real_governors)" "$REAL_BEFORE" "the real governors are unchanged by part 1"

# ------------------------------------------------- part 2: /sdcard semantics

echo
echo "== part 2: the status file's temp+rename on the emulated /sdcard"
U=/sdcard/Android/yc/uperf
printf 'm9-rename-test\n' >"$U/.m9_rename_test.new" 2>/dev/null
if mv "$U/.m9_rename_test.new" "$U/.m9_rename_test" 2>/dev/null; then
    eq "$(cat "$U/.m9_rename_test" 2>/dev/null)" "m9-rename-test" "temp+rename onto /sdcard sticks and reads back"
else
    bad "temp+rename onto /sdcard sticks and reads back"
fi
rm -f "$U/.m9_rename_test" 2>/dev/null
[ -e "$U/.m9_rename_test" ] && c=1 || c=0
check "rm through /sdcard removed it (the FUSE unlink caveat from M6b)" "$c"
if [ "$c" != "0" ]; then
    rm -f /data/media/0/Android/yc/uperf/.m9_rename_test 2>/dev/null
    [ -e "$U/.m9_rename_test" ] && c=1 || c=0
    check "removing it through /data/media/0 worked instead" "$c"
fi

# ------------------------------------- part 3: the dead-man switch, for real

fake_tree() {
    for p in $POLICIES; do
        mkdir -p "$FAKE_FREQ/$p"
        echo schedutil >"$FAKE_FREQ/$p/scaling_governor"
        : >"$FAKE_FREQ/$p/scaling_setspeed"
    done
    rm -f "$T/orig_governor.txt" "$T/uperf.state" "$T/daemon_log.txt"
}
fake_govs() {
    for p in $POLICIES; do
        echo -n "$p=$(cat "$FAKE_FREQ/$p/scaling_governor" 2>/dev/null) "
    done
}

# Start the copied binary with the takeover on, but every write redirected into the
# fake tree. Arms when policy0 reads `userspace` there.
start_test_daemon() {
    local cfg="${DAEMON_CFG:-$T/t3.json}"
    fake_tree
    UPERF_FAKE_ROOT="$FAKE_ROOT" \
        UPERF_CPU_GOVERNOR=1 \
        UPERF_SCHED_DRY_RUN=1 \
        UPERF_LOG_MAX_BYTES="${UPERF_TEST_LOG_MAX:-4194304}" \
        UPERF_STATE_FILE="$T/orig_governor.txt" \
        UPERF_STATUS_FILE="$T/uperf.state" \
        UPERF_STATUS_CONFIG="$cfg" \
        "$FAKE_EXE" "$cfg" -o "$T/daemon_log.txt" </dev/null >/dev/null 2>&1 &
    i=0
    while [ "$i" -lt 60 ]; do
        [ "$(cat "$FAKE_FREQ/policy0/scaling_governor" 2>/dev/null)" = "userspace" ] && return 0
        sleep 1
        i=$((i + 1))
    done
    echo "   !! the test daemon never armed; last log lines:"
    tail -8 "$T/daemon_log.txt" 2>/dev/null | sed 's/^/      | /'
    return 1
}

# The test pair, by image and role (same rule as the watchdog).
find_pair() {
    TSUP=""
    TWORK=""
    for p in $(exe_procs "$FAKE_EXE"); do
        if [ "$(ppid_of "$p")" = "1" ]; then TSUP="$TSUP $p"; else TWORK="$TWORK $p"; fi
    done
    TSUP="${TSUP# }"
    TWORK="${TWORK# }"
}

# `USER_PATH` is $T — the directory the daemon wrote `orig_governor.txt` into, which
# is what `uperf_restore_governors` reads. Only the log/state move per case.
run_watchdog() { # run_watchdog <statedir> <max_restarts> [interval]
    env UPERF_WATCHDOG_EXE="$FAKE_EXE" \
        UPERF_WATCHDOG_CPUFREQ_ROOT="$FAKE_FREQ" \
        UPERF_WATCHDOG_PROC_ROOT=/proc \
        UPERF_WATCHDOG_USER_PATH="$T" \
        UPERF_WATCHDOG_FLAG_PATH="$T/flag" \
        UPERF_WATCHDOG_LOG="$1/wd.log" \
        UPERF_WATCHDOG_STATE="$1/wd.state" \
        UPERF_WATCHDOG_INTERVAL="${3:-2}" \
        UPERF_WATCHDOG_RETRY_INTERVAL=2 \
        UPERF_WATCHDOG_GRACE=1 \
        UPERF_WATCHDOG_MAX_RESTARTS="$2" \
        UPERF_WATCHDOG_TEARDOWN_TICKS="${4:-8}" \
        UPERF_WATCHDOG_VERIFY_WAIT=2 \
        sh "$T/script/uperf_watchdog.sh" >"$1/out.log" 2>&1 &
    echo $!
}

echo
echo "== part 3a: daemon SIGKILLed while armed -> the script-side restore saves it"
if start_test_daemon; then
    echo "   armed: $(fake_govs)"
    has "$T/orig_governor.txt" "policy0 schedutil" "the daemon recorded the originals it replaced"
    has "$T/uperf.state" "armed=3" "the status file reports three armed policies"
    for p in $(exe_procs "$FAKE_EXE"); do kill -KILL "$p" 2>/dev/null; done
    sleep 2
    eq "$(exe_procs "$FAKE_EXE" | wc -l | tr -d ' ')" "0" "both processes are gone (the crash the watchdog exists for)"
    eq "$(fake_govs)" "policy0=userspace policy4=userspace policy7=userspace " "the takeover is still armed afterwards"
    WD3="$(run_watchdog "$T/p3" 0)"
    sleep 14
    kill -TERM "$WD3" 2>/dev/null
    wait "$WD3" 2>/dev/null
    sleep 1
    has "$T/p3/wd.log" "takeover still armed" "the watchdog saw an armed takeover with no daemon"
    has "$T/p3/wd.log" "restore: uperf: restored 3 cpu governor" "it restored all three through the real function"
    eq "$(fake_govs)" "policy0=schedutil policy4=schedutil policy7=schedutil " "every fake policy is back to schedutil"
    eq "$(uperf_zombies)" "0" "no zombie left behind"
else
    bad "the test daemon armed into the fake tree (3a)"
fi

echo
echo "== part 3b: supervisor SIGKILLed -> the orphaned worker disarms on SIGTERM"
if start_test_daemon; then
    find_pair
    OLD_SUP="$TSUP"
    OLD_WORK="$TWORK"
    echo "   supervisor=[$OLD_SUP] worker=[$OLD_WORK]"
    for p in $OLD_SUP; do kill -KILL "$p" 2>/dev/null; done
    sleep 2
    find_pair
    eq "$TWORK" "" "no worker left: the survivor is classified as a lone supervisor"
    eq "$TSUP" "$OLD_WORK" "the survivor is the old worker (pid $OLD_WORK)"
    WD4="$(run_watchdog "$T/p3" 0)"
    sleep 14
    kill -TERM "$WD4" 2>/dev/null
    wait "$WD4" 2>/dev/null
    sleep 1
    has "$T/p3/wd.log" "unhealthy" "the orphan was seen as unhealthy"
    # Keep this case's daemon log: part 4 wipes the shared path, and the point here
    # is that *this* daemon disarmed itself after the *watchdog's* SIGTERM.
    cp -f "$T/daemon_log.txt" "$T/p3/daemon_log_3b.txt" 2>/dev/null
    has "$T/p3/wd.log" "SIGTERM -> \[$OLD_WORK\]" "the watchdog retired the orphan with SIGTERM (pid $OLD_WORK)"
    has "$T/p3/daemon_log_3b.txt" "cpu governor disarmed" "the orphan handed the policies back itself"
    eq "$(fake_govs)" "policy0=schedutil policy4=schedutil policy7=schedutil " "every fake policy is back to schedutil"
    eq "$(uperf_zombies)" "0" "no zombie left behind"
else
    bad "the test daemon armed into the fake tree (3b)"
fi

# --------------------------------------------------- part 4: cost + control

echo
echo "== part 4: watchdog cost, and the control that the real CPU was never touched"
# The log volume is config-driven (`modules.log.level`). `trace` guarantees far more
# than the 64 KiB cap inside the 60 s window: at `info` the volume sat right at the cap
# (measured 39 KB in one run, 263 KB in another) and the assertion was a coin flip.
# 8 KiB: the rotation then happens within seconds of the first lines. A cap at or
# above the daemon's ~30-60 KB/min output made the assertion a coin flip (measured:
# 39 KB, 56 KB, 57 KB in three 60 s runs, never a rotation; the volume is
# activity-bound, so raising `modules.log.level` to `trace` did not change it either).
UPERF_TEST_LOG_MAX=8192
if start_test_daemon; then
    # The shipped cadence: 15 s between samples. Measured against the naive scan
    # (`readlink|sed` per pid) this used to be ~20 s of cpu per sample, i.e. the
    # loop could never keep up; the comm-filtered scan is what makes 15 s a real
    # interval.
    WD5="$(run_watchdog "$T/p3" 0 15)"
    sleep 3
    T0="$(ticks_of "$WD5")"
    sleep 60
    T1="$(ticks_of "$WD5")"
    kill -TERM "$WD5" 2>/dev/null
    wait "$WD5" 2>/dev/null
    [ -n "$T0" ] && [ -n "$T1" ] && c=0 || c=1
    check "read the watchdog's own cpu time" "$c"
    # The daemon bounds its own log (spdlog's rotating sink). 60 s at info level
    # writes far more than 64 KiB, so the rotation must be visible: the base file
    # stops at the cap and `.1` exists. Without this the log grew to 34 MB/day and
    # left a 138 MB `.bak` beside it (measured).
    # This vendored spdlog inserts the index before the extension: `log.txt` rotates
    # to `log.1.txt`, not `log.txt.1` (see rotating_file_sink.h's own comment).
    [ -f "$T/daemon_log.1.txt" ] && c=0 || c=1
    check "the daemon rotated its log (daemon_log.1.txt exists)" "$c"
    base="$(wc -c <"$T/daemon_log.txt" 2>/dev/null | tr -d ' ')"
    [ -n "$base" ] && [ "$base" -le 131072 ] && c=0 || c=1
    check "the base log stayed under the cap (${base:-?} bytes <= 128 KiB)" "$c"

    ms=$(( (T1 - T0) * 10 ))
    echo "   watchdog cpu: $((T1 - T0)) ticks = ${ms} ms over 60 s at the shipped 15 s cadence -> $((ms * 100 / 60000)).$((ms * 100000 / 60000 % 1000)) % of one core (CLK_TCK=100)"
    [ $((T1 - T0)) -le 120 ] && c=0 || c=1
    check "under 1.2 s of cpu in 60 s (2% of one core) at the shipped cadence" "$c"
    for p in $(exe_procs "$FAKE_EXE"); do kill -TERM "$p" 2>/dev/null; done
    sleep 2
    for p in $(exe_procs "$FAKE_EXE"); do kill -KILL "$p" 2>/dev/null; done
    sleep 1
    eq "$(exe_procs "$FAKE_EXE" | wc -l | tr -d ' ')" "0" "the test daemon is fully cleaned up"
else
    bad "the test daemon armed into the fake tree (4)"
fi

eq "$(real_governors)" "$REAL_BEFORE" "the real cpufreq governors are byte-identical to before"

echo
echo "== M9 device verification: $PASS passed, $FAIL failed"
echo "   harness dir: $T"
# A marker, so a truncated log (dropped adb link, killed reader) cannot be mistaken
# for a completed run — the failure mode that cost two device rounds here.
echo "completed=$(date '+%H:%M:%S') passed=$PASS failed=$FAIL" >"$T/.completed"
[ "$FAIL" = "0" ] || exit 1
exit 0
