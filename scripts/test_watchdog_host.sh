#!/bin/sh
#
# Host harness for the M9 watchdog (`magisk/script/uperf_watchdog.sh`).
#
# The watchdog is the reaction to the two exits no in-process path survives
# (SIGKILL, and a Rust panic under `panic = "abort"`): it notices that the daemon
# is gone while the `userspace` takeover is still visible, restores the recorded
# governors, and either restarts the daemon or gives up. That policy is worth
# testing before it ever reaches a phone, so this harness drives the real script
# against fake inputs:
#
#   * a fake cpufreq tree (`UPERF_WATCHDOG_CPUFREQ_ROOT`) — so "armed" is a file
#     we control and no host policy is ever touched;
#   * a copied shell binary as the "module binary" (`UPERF_WATCHDOG_EXE`) — the
#     watchdog identifies processes by `/proc/<pid>/exe`, and a unique image path is
#     what makes a real /proc usable without faking one;
#   * a pair of processes with the dfps topology (supervisor + its forked worker,
#     same image) as the fake daemon;
#   * a stub `uperf_start` (`UPERF_WATCHDOG_STUB`) — the real one launches the
#     device daemon and writes /dev/cpuset. `uperf_restore_governors` stays real, so
#     the restore path is exercised end to end.
#
# Run:  sh scripts/test_watchdog_host.sh        (host only; needs timeout(1))
#
# Not covered here (needs a device): that `/proc/<pid>/exe` really survives dfps'
# cmdline rewrite, that a root `rename` onto /sdcard sticks, and the real daemon's
# SIGTERM disarm. Those are the device items in docs/m9-watchdog.md.

set -u

BASE="$(cd "$(dirname "$0")/.." && pwd)"
WATCHDOG="$BASE/magisk/script/uperf_watchdog.sh"

[ -f "$WATCHDOG" ] || { echo " !! watchdog script not found at $WATCHDOG"; exit 1; }
command -v timeout >/dev/null 2>&1 || { echo " !! timeout(1) is required"; exit 1; }

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
check_contains() { # check_contains <file> <pattern> <description>
    if [ -f "$1" ] && grep -q "$2" "$1"; then
        ok "$3"
    else
        bad "$3 (no match for '$2' in $(basename "$1"))"
        [ -f "$1" ] && sed 's/^/        | /' "$1"
    fi
}
check_missing() { # check_missing <file> <pattern> <description>
    if [ ! -f "$1" ] || ! grep -q "$2" "$1"; then
        ok "$3"
    else
        bad "$3 (unexpected match for '$2' in $(basename "$1"))"
    fi
}
eq() { # eq <actual> <expected> <description>
    if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (got '$1', want '$2')"; fi
}

# ---------------------------------------------------------------- fixtures

WORK="$(mktemp -d "${TMPDIR:-/tmp}/uperf_watchdog_test.XXXXXX")"
# Named `uperf`, not `fake_uperf`: the watchdog filters candidates by the process
# name the daemon sets for itself (`DAEMON_COMM`, default `uperf`), which is
# independent of the image's file name. Naming it anything else would only test the
# harness, not the module.
EXE="$WORK/uperf"
# A copy (not a symlink) of the shell: /proc/<pid>/exe then reports *this* path,
# which is what the watchdog matches on. `sh` itself must NOT match, or the
# harness and the watchdog would be classified as daemon processes.
cp -L "$(readlink -f "$(command -v sh)")" "$EXE"
chmod +x "$EXE"

# The fake daemon body: fork a worker with the same image, remember its pid, wait.
# That is the dfps topology — supervisor = the process whose parent is not one of
# ours, worker = its child.
cat >"$WORK/fake_daemon.sh" <<'DAEMON'
"$FAKE_EXE" -c 'while :; do sleep 1; done' &
echo "$!" >"$FAKE_PIDFILE"
wait
DAEMON

# Start it detached and record both pids (worker first, supervisor last), so a
# caller can wait for a complete pair. Used both by the harness and by the stub's
# "successful restart".
cat >"$WORK/start_fake.sh" <<'HELPER'
exe="$1"
pidfile="$2"
work="$3"
FAKE_EXE="$exe" FAKE_PIDFILE="$pidfile" "$exe" "$work/fake_daemon.sh" &
sup="$!"
i=0
while [ "$i" -lt 50 ]; do
    [ -s "$pidfile" ] && break
    sleep 0.1
    i=$((i + 1))
done
echo "$sup" >>"$pidfile"
HELPER

cleanup() {
    for f in "$WORK"/*/pids/*; do
        [ -f "$f" ] || continue
        while read -r p; do kill -KILL "$p" 2>/dev/null; done <"$f"
    done
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# Any process still running the fake image. Cases must not leak into one another:
# a leftover pair makes a later case see two supervisors, which the watchdog
# (correctly) treats as unhealthy — that would make the harness test the wrong thing.
kill_leftover_fakes() {
    for p in /proc/[0-9]*; do
        pid="${p##*/}"
        [ "$(readlink "$p/exe" 2>/dev/null | sed 's/ (deleted)$//')" = "$EXE" ] || continue
        kill -KILL "$pid" 2>/dev/null
    done
    sleep 0.2
}

new_case() { # new_case <name> [policy...]
    name="$1"
    shift
    kill_leftover_fakes
    # Per-case knobs are globals: reset them here or one case silently changes the
    # next one's premise (observed: case 7's state path leaked into case 9, and
    # case 3's "bring a daemon up" leaked into every later case).
    WD_VERIFY=1
    WD_STATE_PATH=""
    WD_BACKSTOP=20
    SF_INJECT=""
    SF_MAX_INJECTS=3
    SF_RETRY_SAMPLES=1
    SF_INJECTOR="$WORK/injector"
    WD_DRY_RUN=0
    STUB_BRINGUP=0
    W="$WORK/$name"
    rm -rf "$W"
    mkdir -p "$W/flag" "$W/user" "$W/pids" "$W/cpufreq"
    : >"$W/user/orig_governor.txt"
    for p in "$@"; do
        mkdir -p "$W/cpufreq/$p"
        echo schedutil >"$W/cpufreq/$p/scaling_governor"
        # What the daemon recorded when it took the policy over (GOVERNOR_STATE).
        echo "$p schedutil" >>"$W/user/orig_governor.txt"
    done
    : >"$W/stub.log"
    cat >"$W/stub.sh" <<'STUB'
uperf_start() {
    echo "called" >>"$UPERF_WATCHDOG_STUB_LOG"
    if [ "${UPERF_WATCHDOG_STUB_BRINGUP:-0}" = "1" ]; then
        sh "$UPERF_WATCHDOG_TEST_HELPER" "$UPERF_WATCHDOG_TEST_EXE" \
            "$UPERF_WATCHDOG_TEST_PIDFILE" "$UPERF_WATCHDOG_TEST_WORK" >/dev/null 2>&1
    fi
    return 0
}
STUB
    : >"$W/wd.log"
    : >"$W/pids/case.pids"
}

# Bring up a fake pair and wait until both are visible.
start_fake_daemon() { # start_fake_daemon <pidfile>
    pidfile="$1"
    : >"$pidfile"
    sh "$WORK/start_fake.sh" "$EXE" "$pidfile" "$WORK"
    i=0
    while [ "$i" -lt 50 ]; do
        [ "$(wc -l <"$pidfile" 2>/dev/null)" -ge 2 ] && break
        sleep 0.1
        i=$((i + 1))
    done
}

pid_worker() { head -n 1 "$1" 2>/dev/null; }
pid_super() { tail -n 1 "$1" 2>/dev/null; }

watchdog_run() { # watchdog_run <seconds> -> prints the pid of timeout(1)
    env \
        UPERF_WATCHDOG_FLAG_PATH="$W/flag" \
        UPERF_WATCHDOG_USER_PATH="$W/user" \
        UPERF_WATCHDOG_CPUFREQ_ROOT="$W/cpufreq" \
        UPERF_WATCHDOG_EXE="$EXE" \
        UPERF_WATCHDOG_LOG="$W/wd.log" \
        UPERF_WATCHDOG_STATE="${WD_STATE_PATH:-$W/wd.state}" \
        UPERF_WATCHDOG_INTERVAL="${WD_INTERVAL:-1}" \
        UPERF_WATCHDOG_RETRY_INTERVAL="${WD_RETRY:-1}" \
        UPERF_WATCHDOG_GRACE="${WD_GRACE:-1}" \
        UPERF_WATCHDOG_MAX_RESTARTS="${WD_MAX_RESTARTS:-2}" \
        UPERF_WATCHDOG_TEARDOWN_TICKS="${WD_TEARDOWN:-2}" \
        UPERF_WATCHDOG_BACKSTOP="${WD_BACKSTOP:-20}" \
        UPERF_SF_INJECT="${SF_INJECT:-}" \
        UPERF_SF_TARGET="${SF_TARGET:-$WORK/surfaceflinger}" \
        UPERF_SF_LIB="${SF_LIB:-$WORK/libsfanalysis_rs.so}" \
        UPERF_SF_INJECTOR="${SF_INJECTOR:-$WORK/injector}" \
        UPERF_SF_MAX_INJECTS="${SF_MAX_INJECTS:-3}" \
        UPERF_SF_STAGE="${SF_STAGE:-$WORK/staged/libsfanalysis_rs.so}" \
        UPERF_SF_RETRY_SAMPLES="${SF_RETRY_SAMPLES:-1}" \
        UPERF_WATCHDOG_VERIFY_WAIT="${WD_VERIFY:-1}" \
        UPERF_WATCHDOG_DRY_RUN="${WD_DRY_RUN:-0}" \
        UPERF_WATCHDOG_STUB="$W/stub.sh" \
        UPERF_WATCHDOG_STUB_LOG="$W/stub.log" \
        UPERF_WATCHDOG_STUB_BRINGUP="${STUB_BRINGUP:-0}" \
        UPERF_WATCHDOG_TEST_EXE="$EXE" \
        UPERF_WATCHDOG_TEST_WORK="$WORK" \
        UPERF_WATCHDOG_TEST_HELPER="$WORK/start_fake.sh" \
        UPERF_WATCHDOG_TEST_PIDFILE="$W/pids/restarted.pids" \
        sh "$WATCHDOG" >"$W/out.log" 2>&1 &
    # The pid of the shell itself, not of a wrapper: the owner lock is written with
    # `$$`, and one assertion compares the two.
    echo "$!"
}

stop_watchdog() { # stop_watchdog <watchdog-pid> — leaves time for the trap + state write
    kill -TERM "$1" 2>/dev/null
    i=0
    while [ "$i" -lt 10 ]; do
        kill -0 "$1" 2>/dev/null || break
        sleep 0.5
        i=$((i + 1))
    done
    kill -KILL "$1" 2>/dev/null
    wait "$1" 2>/dev/null
    sleep 1
}

alive() { kill -0 "$1" 2>/dev/null; }

state_field() { sed -n "s/^$2=//p" "$WORK/$1/wd.state" 2>/dev/null | head -n 1; }

governor_of() { cat "$WORK/$1/cpufreq/$2/scaling_governor" 2>/dev/null; }

# ---------------------------------------------------------------- case 1: healthy

echo "== case 1: a healthy daemon is left alone"
new_case healthy policy0 policy4
start_fake_daemon "$WORK/healthy/pids/case.pids"
SUP="$(pid_super "$WORK/healthy/pids/case.pids")"
WORKER="$(pid_worker "$WORK/healthy/pids/case.pids")"
WD_PID="$(watchdog_run 8)"
sleep 4
check_contains "$WORK/healthy/wd.log" "healthy" "log reports a healthy daemon"
check_contains "$WORK/healthy/wd.log" "armed=\[\]" "the live (unarmed) policy state is reported"
check_contains "$WORK/healthy/wd.log" "watchdog started" "the startup line is written"
eq "$(state_field healthy state)" "running" "state file says running"
alive "$SUP" && c=0 || c=1
check "the fake supervisor was not killed" "$c"
alive "$WORKER" && c=0 || c=1
check "the fake worker was not killed" "$c"
[ ! -s "$WORK/healthy/stub.log" ] && c=0 || c=1
check "no restart was attempted" "$c"
# Liveness must be readable from the file, not only from the lock: the timestamp is
# rewritten every sample even when nothing changes.
m1="$(state_field healthy updated_uptime_ms)"
sleep 3
m2="$(state_field healthy updated_uptime_ms)"
[ -n "$m1" ] && [ -n "$m2" ] && [ "$m2" -gt "$m1" ] && c=0 || c=1
check "the state file's timestamp advances while healthy ($m1 -> $m2)" "$c"
stop_watchdog "$WD_PID"
eq "$(state_field healthy state)" "stopped" "a clean stop records state=stopped"
kill -KILL "$SUP" "$WORKER" 2>/dev/null

# ------------------------------------------------- case 2: dead daemon + armed

echo "== case 2: dead daemon while armed -> budgeted restarts, then restore"
new_case deadarmed policy0 policy4
printf 'userspace' >"$WORK/deadarmed/cpufreq/policy0/scaling_governor"
# The stub never brings a daemon up, so there is nothing to wait for after a
# restart: verify immediately and let the budget burn down faster.
WD_VERIFY=0
WD_PID="$(watchdog_run 40)"
sleep 20
# Read the live state before stopping: a stop legitimately rewrites it (see the
# assertion on the detail line below).
eq "$(state_field deadarmed state)" "gave-up" "state file says gave-up while it is still running"
stop_watchdog "$WD_PID"
eq "$(wc -l <"$WORK/deadarmed/stub.log" 2>/dev/null)" "2" "restarts exactly the budget (2)"
eq "$(governor_of deadarmed policy0)" "schedutil" "the armed policy was restored to schedutil"
eq "$(governor_of deadarmed policy4)" "schedutil" "the policy that was never taken over is untouched"
eq "$(state_field deadarmed state)" "stopped" "a stop after a give-up records stopped"
check_contains "$WORK/deadarmed/wd.state" "gave-up" "the stop keeps the give-up in the detail"
check_contains "$WORK/deadarmed/wd.log" "restore: uperf: restored 1 cpu governor" \
    "the real restore function ran and said so"
check_contains "$WORK/deadarmed/wd.log" "restart budget spent" "the give-up is logged"

# ------------------------------------------- case 3: dead daemon, restart works

echo "== case 3: dead daemon, restart brings it back"
new_case restartok policy0
printf 'userspace' >"$WORK/restartok/cpufreq/policy0/scaling_governor"
STUB_BRINGUP=1
WD_PID="$(watchdog_run 20)"
sleep 8
calls="$(wc -l <"$WORK/restartok/stub.log" 2>/dev/null)"
[ "${calls:-0}" -ge 1 ] && c=0 || c=1
check "a restart was attempted ($calls)" "$c"
eq "$(state_field restartok state)" "running" "state file says running again"
SUP2="$(pid_super "$WORK/restartok/pids/restarted.pids")"
[ -n "$SUP2" ] && alive "$SUP2" && c=0 || c=1
check "the restarted daemon is alive (pid=$SUP2)" "$c"
check_missing "$WORK/restartok/wd.log" "gave-up" "the budget was not burned"
stop_watchdog "$WD_PID"
kill -KILL "$SUP2" "$(pid_worker "$WORK/restartok/pids/restarted.pids")" 2>/dev/null

# ------------------------------------------------- case 4: orphaned worker only

echo "== case 4: killed supervisor, orphaned worker -> the orphan is retired"
new_case orphan policy0
start_fake_daemon "$WORK/orphan/pids/case.pids"
SUP="$(pid_super "$WORK/orphan/pids/case.pids")"
WORKER="$(pid_worker "$WORK/orphan/pids/case.pids")"
printf 'userspace' >"$WORK/orphan/cpufreq/policy0/scaling_governor"
kill -KILL "$SUP" 2>/dev/null
sleep 1
alive "$WORKER" && c=0 || c=1
check "the worker really outlived its supervisor (pid=$WORKER)" "$c"
WD_PID="$(watchdog_run 30)"
sleep 14
stop_watchdog "$WD_PID"
alive "$WORKER" && c=1 || c=0
check "the orphan was taken down" "$c"
eq "$(governor_of orphan policy0)" "schedutil" "the armed policy was restored"
check_contains "$WORK/orphan/wd.log" "unhealthy" "the missing supervisor was noticed"

# ------------------------------------------------------- case 5: single instance

echo "== case 5: one watchdog only (owner lock)"
new_case lock
start_fake_daemon "$WORK/lock/pids/case.pids"
SUP="$(pid_super "$WORK/lock/pids/case.pids")"
WORKER="$(pid_worker "$WORK/lock/pids/case.pids")"
WD_A="$(watchdog_run 10)"
sleep 2
owner="$(cat "$WORK/lock/flag/uperf_watchdog.lock/owner" 2>/dev/null)"
eq "${owner%%:*}" "$WD_A" "the lock names the live watchdog (owner='$owner')"
env \
    UPERF_WATCHDOG_FLAG_PATH="$W/flag" \
    UPERF_WATCHDOG_USER_PATH="$W/user" \
    UPERF_WATCHDOG_CPUFREQ_ROOT="$W/cpufreq" \
    UPERF_WATCHDOG_EXE="$EXE" \
    UPERF_WATCHDOG_LOG="$W/wd.log" \
    UPERF_WATCHDOG_STATE="$W/wd.state" \
    UPERF_WATCHDOG_STUB="$W/stub.sh" \
    UPERF_WATCHDOG_INTERVAL=1 \
    timeout 5 sh "$WATCHDOG" >"$WORK/lock/second.out" 2>&1
rc=$?
eq "$rc" "0" "a second instance exits instead of supervising"
check_contains "$WORK/lock/wd.log" "another watchdog owns the lock" "the second instance says why"
alive "$SUP" && c=0 || c=1
check "the second instance did not touch the daemon" "$c"
stop_watchdog "$WD_A"
[ -d "$WORK/lock/flag/uperf_watchdog.lock" ] && c=1 || c=0
check "the lock is released on the way out" "$c"
kill -KILL "$SUP" "$WORKER" 2>/dev/null

# ------------------------------------------- case 7: the WebUI status contract

echo "== case 7: the WebUI status reports the M9 state"
new_case webui policy0
cat >"$WORK/webui/user/uperf.state" <<'STATE'
version=1
state=running
takeover=on
armed=1
policies=policy0
pid=1234
STATE
start_fake_daemon "$WORK/webui/pids/case.pids"
# Here the watchdog must write its status file where the WebUI reads it, i.e. the
# default `$USER_PATH/uperf_watchdog.state` rather than the harness's own path.
WD_STATE_PATH="$W/user/uperf_watchdog.state"
WD_PID="$(watchdog_run 10)"
sleep 3
OUT="$WORK/webui/status.txt"
env UPERF_WEBUI_USER_PATH="$W/user" UPERF_WEBUI_FLAG_PATH="$W/flag" \
    sh "$BASE/magisk/script/webui.sh" status >"$OUT" 2>&1
rc=$?
eq "$rc" "0" "status exits 0"
check_contains "$OUT" "^daemon.state=running$" "status exposes daemon.state"
check_contains "$OUT" "^daemon.armed=1$" "status exposes daemon.armed"
check_contains "$OUT" "^watchdog.state=running$" "status exposes watchdog.state"
check_contains "$OUT" "^watchdog.restarts=0$" "status exposes watchdog.restarts"
check_contains "$OUT" "^watchdog.pid=$WD_PID$" "status exposes the live watchdog pid"
check_contains "$OUT" "^governor.takeover=0$" "the pre-existing governor keys still print"
check_contains "$OUT" "^boot.id=" "the pre-existing boot key still prints"
stop_watchdog "$WD_PID"
kill -KILL "$(pid_super "$WORK/webui/pids/case.pids")" "$(pid_worker "$WORK/webui/pids/case.pids")" 2>/dev/null

# ------------------------------------------- case 8: two supervisors collapse

echo "== case 8: two supervisors (a raced restart) are collapsed"
new_case dup policy0
start_fake_daemon "$WORK/dup/pids/a.pids"
start_fake_daemon "$WORK/dup/pids/b.pids"
printf 'userspace' >"$WORK/dup/cpufreq/policy0/scaling_governor"
WD_PID="$(watchdog_run 20)"
sleep 8
stop_watchdog "$WD_PID"
check_contains "$WORK/dup/wd.log" "sup_n=2" "two supervisors are detected as unhealthy"
still=0
for p in $(cat "$WORK/dup/pids/a.pids" 2>/dev/null) $(cat "$WORK/dup/pids/b.pids" 2>/dev/null); do
    alive "$p" && still=$((still + 1))
done
eq "$still" "0" "every supervisor/worker pair was retired"
eq "$(governor_of dup policy0)" "schedutil" "the takeover was undone"
check_contains "$WORK/dup/stub.log" "called" "a single daemon was restarted"

# ------------------------------- case 11: the steady state does not sweep every sample

echo "== case 11: a healthy pair is answered from the cache, not by a sweep per sample"
new_case steady policy0
start_fake_daemon "$WORK/steady/pids/a.pids"
printf 'userspace' >"$WORK/steady/cpufreq/policy0/scaling_governor"
WD_INTERVAL=1
WD_PID="$(watchdog_run 7)"
sleep 6
# Read before stopping: the stop path rewrites the file as `state=stopped` (which is
# the point of the file, and would make this assertion test the wrong thing).
cp "$WORK/steady/wd.state" "$WORK/steady/wd.state.at_run"
stop_watchdog "$WD_PID"
eq "$(sed -n 's/^state=//p' "$WORK/steady/wd.state.at_run")" "running" "the pair is still reported healthy"
eq "$(sed -n 's/^sweeps=//p' "$WORK/steady/wd.state.at_run")" "1" "the whole run cost exactly one /proc sweep"
check_contains "$WORK/steady/wd.state.at_run" "^since_sweep=[1-9][0-9]*$" "samples since the sweep are counted"
check_contains "$WORK/steady/wd.state.at_run" "^backstop=20$" "the shipped backstop is what ran"
check_contains "$WORK/steady/wd.log" "healthy:" "the healthy transition was still logged"
kill -KILL "$(pid_super "$WORK/steady/pids/a.pids")" "$(pid_worker "$WORK/steady/pids/a.pids")" 2>/dev/null

# ------------------------------ case 12: the daemon's sysfs write ledger

echo "== case 12: sysfs knobs are put back; an unrecorded one is only reported"
new_case sysfs
ROOT="$WORK/sysfs/root"
mkdir -p "$ROOT/sys/module/msm_performance/parameters" "$ROOT/sys/kernel"
KNOB="$ROOT/sys/module/msm_performance/parameters/cpu_max_freq"
printf '1612800\n' >"$KNOB"                    # what the daemon's write left behind
printf '0-3\n' >"$ROOT/sys/kernel/already_ok"  # recorded, and already correct
printf '999\n' >"$ROOT/sys/kernel/left_alone"  # the ledger never mentions this one
mkdir -p "$ROOT/sys/kernel/not_writable"       # a directory: the restore must fail here
LEDGER="$WORK/sysfs/sysfs_orig.txt"
{
    echo '# a comment line the loader must skip'
    echo '/sys/module/msm_performance/parameters/cpu_max_freq 2419200'
    echo '/sys/kernel/already_ok 0-3'
    echo '/sys/ro/path'
    echo '/sys/kernel/not_writable 7'
    echo 'garbage with no path'
} >"$LEDGER"

restore() {
    # `libuperf.sh` sources `./pathinfo.sh` relative to `$0`, so it has to be sourced
    # from its own directory (a `sh -c` would make `$0` = `sh` and the relative source
    # would resolve against the caller's cwd).
    env UPERF_SYSFS_ORIG="$LEDGER" UPERF_SYSFS_ROOT="$ROOT" \
        sh -c "cd \"$(dirname "$WATCHDOG")\" && . ./libuperf.sh && uperf_restore_sysfs" \
        >"$WORK/sysfs/out.$1.log" 2>&1
    return $?
}
restore 1
OUT="$WORK/sysfs/out.1.log"
check_contains "$OUT" "^uperf: restored 1 sysfs knob" "the changed knob was restored"
check_contains "$OUT" "already at their original value" "an already-correct knob is only counted"
check_contains "$OUT" "no recorded original for /sys/ro/path" "a path with no recorded original is reported"
check_contains "$OUT" "could not restore /sys/kernel/not_writable" "a refused write is reported"
check_contains "$OUT" "ignoring unreadable line in .*: garbage with no path" "a line that is not an absolute path is ignored and reported, not written to"
check_contains "$OUT" "1 unreadable ledger line" "the ignored line is counted"
eq "$(cat "$KNOB")" "2419200" "the recorded knob is back to its original value"
eq "$(cat "$ROOT/sys/kernel/already_ok")" "0-3" "the untouched knob is byte-identical still"
eq "$(cat "$ROOT/sys/kernel/left_alone")" "999" "a path the ledger never names is not touched"
[ -f "$LEDGER" ] && ok "the ledger is kept while an entry is still owed" || bad "the ledger was cleared with an entry still owed"

# Clear the two stragglers: then nothing is owed and the ledger must go.
rm -rf "$ROOT/sys/kernel/not_writable"
printf '7\n' >"$ROOT/sys/kernel/not_writable"
sed -i '/^\/sys\/ro\/path$/d;/^garbage/d' "$LEDGER"
restore 2
check_missing "$WORK/sysfs/out.2.log" "kept at" "nothing is left owed"
[ -f "$LEDGER" ] && bad "the ledger survived a clean restore" || ok "the ledger is cleared once nothing is owed"

# ------------------------ case 13: SF injection supervision (M8, fas-rs' re-attach)

echo "== case 13: a frame source that disappears is re-attached, counted, and given up on"
new_case sf policy0
SF="$WORK/surfaceflinger"
cp -f "$EXE" "$SF" 2>/dev/null   # a copy of the shell under the name whose comm we filter on
[ -x "$SF" ] || cp -f "$(command -v sh)" "$SF"
: >"$WORK/injector"; chmod 755 "$WORK/injector"
cat >>"$WORK/sf/stub.sh" <<'SFSTUB'

# --- test overrides for the M8 injection supervision (sourced after the definitions) ---
wd_sf_is_injected() { [ -f "$UPERF_WATCHDOG_STUB_LOG.injected.$1" ]; }
wd_sf_do_inject() {
    echo "inject pid=$1" >>"$UPERF_WATCHDOG_STUB_LOG.sfinject"
    touch "$UPERF_WATCHDOG_STUB_LOG.injected.$1"
    return 0
}
SFSTUB
# The staging step copies the library to a path the target can read (`chown`/`chcon` are
# best effort and absent on a host, the copy is not).
echo "lib" >"$WORK/libsfanalysis_rs.so"
SF_STAGE="$WORK/staged/libsfanalysis_rs.so"
export SF_STAGE

start_sf() {
    rm -f "$WORK"/sf*/stub.log.injected* 2>/dev/null
    "$SF" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
    echo $! >"$WORK/sf.pid"
    sleep 1
}

# off by default: the sample costs a string test and nothing else
SF_INJECT=""
WD_INTERVAL=1
WD_PID="$(watchdog_run 4)"
sleep 3
stop_watchdog "$WD_PID"
cp "$WORK/sf/wd.state" "$WORK/sf/off.state"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf/off.state")" "off" "injection supervision is off unless asked for"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf/off.state")" "0" "and it injected nothing"

# on, with no surfaceflinger running
new_case sf2 policy0
cp -f "$EXE" "$WORK/surfaceflinger" 2>/dev/null || true
: >"$WORK/injector"; chmod 755 "$WORK/injector"
cat >>"$WORK/sf2/stub.sh" <<'SFSTUB'
wd_sf_is_injected() { [ -f "$UPERF_WATCHDOG_STUB_LOG.injected.$1" ]; }
wd_sf_do_inject() {
    echo "inject pid=$1" >>"$UPERF_WATCHDOG_STUB_LOG.sfinject"
    touch "$UPERF_WATCHDOG_STUB_LOG.injected.$1"
    return 0
}
SFSTUB
SF_INJECT=1
WD_INTERVAL=1
start_fake_daemon "$WORK/sf2/pids/a.pids"
WD_PID="$(watchdog_run 4)"
sleep 2
cp "$WORK/sf2/wd.state" "$WORK/sf2/absent.state"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf2/absent.state")" "absent" "no target running is reported as absent"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf2/absent.state")" "0" "and nothing was attempted"

# the target appears: one injection, then a healthy no-op
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID=$!
sleep 5
cp "$WORK/sf2/wd.state" "$WORK/sf2/injected.state"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf2/injected.state")" "injected" "the first sight of the target injects once"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf2/injected.state")" "1" "exactly one injection so far"
sleep 4
cp "$WORK/sf2/wd.state" "$WORK/sf2/steady.state"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf2/steady.state")" "1" "a healthy mapping is not re-injected"
eq "$(grep -c '^inject' "$WORK/sf2/stub.log.sfinject" 2>/dev/null)" "1" "the injector was called once in total"

# the target restarts -> a new identity -> re-attach
kill -KILL "$SFPID" 2>/dev/null
sleep 2
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID2=$!
sleep 5
cp "$WORK/sf2/wd.state" "$WORK/sf2/restart.state"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf2/restart.state")" "2" "a restarted target is re-injected"
check_contains "$WORK/sf2/wd.log" "restarted" "and the restart is what the log blames"

# More restarts are still served: the budget counts failures, not attempts, so a device
# whose surfaceflinger restarts often does not lose its frame source for the rest of boot.
kill -KILL "$SFPID2" 2>/dev/null
sleep 2
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID3=$!
sleep 5
cp "$WORK/sf2/wd.state" "$WORK/sf2/third.state"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf2/third.state")" "3" "a third restart is still injected (successes do not spend the budget)"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf2/third.state")" "injected" "and the state says the mapping is ours"
kill -KILL "$SFPID" "$SFPID2" "$SFPID3" 2>/dev/null
for p in $(pgrep -f "$WORK/surfaceflinger" 2>/dev/null); do kill -KILL "$p" 2>/dev/null; done
stop_watchdog "$WD_PID"

# failures do spend it: an injector that keeps failing must stop being retried
new_case sf4 policy0
cp -f "$EXE" "$WORK/surfaceflinger" 2>/dev/null || true
: >"$WORK/injector"; chmod 755 "$WORK/injector"
cat >>"$WORK/sf4/stub.sh" <<'SFSTUB'
wd_sf_is_injected() { [ -f "$UPERF_WATCHDOG_STUB_LOG.injected.$1" ]; }
wd_sf_do_inject() {
    echo "fail pid=$1" >>"$UPERF_WATCHDOG_STUB_LOG.sfinject"
    return 1
}
SFSTUB
SF_INJECT=1
WD_INTERVAL=1
start_fake_daemon "$WORK/sf4/pids/a.pids"
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID4=$!
WD_PID="$(watchdog_run 20)"
sleep 12
cp "$WORK/sf4/wd.state" "$WORK/sf4/gaveup.state"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf4/gaveup.state")" "gave-up" "a failing injector is given up on, not retried forever"
eq "$(sed -n 's/^sf_fails=//p' "$WORK/sf4/gaveup.state")" "3" "the failure count is what the budget spent"
check_contains "$WORK/sf4/wd.log" "gave up after 3 failed injection attempt" "the give-up says why, failures included"
eq "$(grep -c '^fail ' "$WORK/sf4/stub.log.sfinject" 2>/dev/null)" "3" "and it stopped after the budget, not at the end of the run"
kill -KILL "$SFPID4" 2>/dev/null
stop_watchdog "$WD_PID"

# no injector at all: disabled once, loudly, instead of failing every sample
new_case sf3 policy0
SF_INJECT=1
SF_INJECTOR="$WORK/definitely-not-here"
WD_INTERVAL=1
start_fake_daemon "$WORK/sf3/pids/a.pids"
# The injector is only consulted when something needs injecting: a target has to be
# running, or the loop correctly says `absent` and never looks at the injector.
cp -f "$EXE" "$WORK/surfaceflinger" 2>/dev/null || true
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID5=$!
WD_PID="$(watchdog_run 4)"
sleep 3
stop_watchdog "$WD_PID"
cp "$WORK/sf3/wd.state" "$WORK/sf3/disabled.state"
check_contains "$WORK/sf3/wd.log" "no usable injector" "a missing injector is reported once"
eq "$(grep -c 'no usable injector' "$WORK/sf3/wd.log" 2>/dev/null)" "1" "and not once per sample"
eq "$(sed -n 's/^sf_state=//p' "$WORK/sf3/wd.state")" "disabled" "and the loop is switched off rather than failing every sample"
eq "$(sed -n 's/^sf_injects=//p' "$WORK/sf3/wd.state")" "0" "with no attempt recorded"
kill -KILL "$SFPID5" 2>/dev/null

# ------------------- case 14: the real inject path stages the library, then injects *it*

echo "== case 14: the library is staged for the target domain, and the staged copy is what gets injected"
new_case sf5 policy0
echo "modulecopy" >"$WORK/libsfanalysis_rs.so"
cat >"$WORK/fakeinjector" <<FI
#!/bin/sh
echo "\$2" >>"$WORK/sf5/injected-path.txt"
exit 0
FI
chmod 755 "$WORK/fakeinjector"
cp -f "$EXE" "$WORK/surfaceflinger" 2>/dev/null || true
start_fake_daemon "$WORK/sf5/pids/a.pids"
SF_INJECT=1
SF_INJECTOR="$WORK/fakeinjector"
WD_INTERVAL=1
"$WORK/surfaceflinger" -c 'while :; do sleep 1; done' >/dev/null 2>&1 &
SFPID6=$!
WD_PID="$(watchdog_run 6)"
sleep 5
cp "$WORK/sf5/wd.state" "$WORK/sf5/staged.state"
eq "$(cat "$SF_STAGE" 2>/dev/null)" "modulecopy" "the library was staged where the target can read it"
eq "$(stat -c %a "$SF_STAGE" 2>/dev/null)" "755" "the staged copy is world-readable (the target domain is not root)"
eq "$(head -1 "$WORK/sf5/injected-path.txt" 2>/dev/null)" "$SF_STAGE" "the injector was pointed at the staged copy, not the module's own"
eq "$(sort -u "$WORK/sf5/injected-path.txt" 2>/dev/null | grep -c .)" "1" "every attempt used that one path (a retry re-stages, never falls back to the module copy)"
eq "$(sed -n 's/^sf_fails=//p' "$WORK/sf5/staged.state")" "1" "a mapping that never appears after a zero exit is a failure, not a success"
check_contains "$WORK/sf5/wd.log" "returned success but the mapping is absent" "and it says exactly that"
kill -KILL "$SFPID6" 2>/dev/null
stop_watchdog "$WD_PID"

# ------------------------------------------------------- case 9: a killed daemon is seen

echo "== case 11b: the cache never hides a kill (the sweep is forced when a pid is gone)"
new_case killed policy0
start_fake_daemon "$WORK/killed/pids/a.pids"
printf 'userspace' >"$WORK/killed/cpufreq/policy0/scaling_governor"
WD_INTERVAL=1
WD_PID="$(watchdog_run 12)"
sleep 2
kill -KILL "$(pid_super "$WORK/killed/pids/a.pids")" "$(pid_worker "$WORK/killed/pids/a.pids")" 2>/dev/null
sleep 8
stop_watchdog "$WD_PID"
eq "$(governor_of killed policy0)" "schedutil" "the kill was noticed from the cache and the takeover undone"
check_contains "$WORK/killed/wd.log" "unhealthy sample" "an unhealthy sample was logged"
check_contains "$WORK/killed/wd.log" "restore:" "the real restore ran"

# ------------------------------------------------------------- case 9: dry run

echo "== case 9: UPERF_WATCHDOG_DRY_RUN=1 reports without acting"
new_case dryrun policy0
printf 'userspace' >"$WORK/dryrun/cpufreq/policy0/scaling_governor"
start_fake_daemon "$WORK/dryrun/pids/case.pids"
SUP="$(pid_super "$WORK/dryrun/pids/case.pids")"
WORKER="$(pid_worker "$WORK/dryrun/pids/case.pids")"
# Kill the supervisor: the pair is now "unhealthy" with the takeover still armed,
# i.e. exactly the state the watchdog would otherwise recover from.
kill -KILL "$SUP" 2>/dev/null
WD_DRY_RUN=1
WD_PID="$(watchdog_run 10)"
sleep 6
stop_watchdog "$WD_PID"
check_contains "$WORK/dryrun/wd.log" "dry_run=1" "the run is marked as a dry run"
check_contains "$WORK/dryrun/wd.log" "unhealthy sample" "the verdict is reported"
eq "$(state_field dryrun state)" "stopped" "no state claim beyond the stop"
check_contains "$WORK/dryrun/wd.state" "last state: unhealthy" "the observed state survives the stop"
alive "$WORKER" && c=0 || c=1
check "the surviving process was NOT retired" "$c"
eq "$(governor_of dryrun policy0)" "userspace" "the armed policy was NOT restored"
[ ! -s "$WORK/dryrun/stub.log" ] && c=0 || c=1
check "no restart was attempted" "$c"
kill -KILL "$WORKER" 2>/dev/null

# ------------------------------------- case 10: the log backup cap (script side)

echo "== case 10: an oversized log backup is dropped, a small one is kept"
new_case logcap
# The real `libuperf.sh`, sourced off-device: `USER_PATH` is redirected right after
# sourcing because the function reads it at call time (the same late-binding rule the
# watchdog needed). `UPERF_LOG_BACKUP_MAX_BYTES` is exported so the sourced default
# does not win.
run_trim() { # run_trim <user-dir> <max-bytes> <out-file>
    (
        cd "$BASE/magisk/script" || exit 1
        UPERF_LOG_BACKUP_MAX_BYTES="$2" sh -c '
            . ./libuperf.sh
            USER_PATH="$1"
            uperf_trim_log_backup
            echo "present=$([ -f "$USER_PATH/uperf_log.txt.bak" ] && echo 1 || echo 0)"
            echo "size=$([ -f "$USER_PATH/uperf_log.txt.bak" ] && wc -c <"$USER_PATH/uperf_log.txt.bak" | tr -d " " || echo 0)"
        ' _ "$1"
    ) >"$3" 2>&1
}

head -c 200000 /dev/zero | tr '\0' 'x' >"$WORK/logcap/user/uperf_log.txt.bak"
run_trim "$WORK/logcap/user" 100000 "$WORK/logcap/big.out"
check_contains "$WORK/logcap/big.out" "present=0" "a backup over the cap is dropped"
check_contains "$WORK/logcap/big.out" "dropped an oversized log backup" "and it says so"

head -c 50 /dev/zero | tr '\0' 'x' >"$WORK/logcap/user/uperf_log.txt.bak"
run_trim "$WORK/logcap/user" 100000 "$WORK/logcap/small.out"
check_contains "$WORK/logcap/small.out" "present=1" "a backup under the cap is kept"
check_contains "$WORK/logcap/small.out" "size=50" "and is left byte-exact"

# ------------------------------------------------------- case 6: syntax + wiring

echo "== case 6: scripts parse and the module wiring is present"
for s in "$WATCHDOG" "$BASE/magisk/script/libuperf.sh" "$BASE/magisk/script/libcommon.sh" \
    "$BASE/magisk/script/webui.sh" "$BASE/magisk/customize.sh" \
    "$BASE/magisk/uninstall.sh" "$BASE/scripts/cpufreq-write-probe.sh"; do
    sh -n "$s" 2>/dev/null && c=0 || c=1
    check "sh -n $(basename "$s")" "$c"
done
check_contains "$BASE/magisk/script/libuperf.sh" "^    uperf_watchdog_start$" "uperf_start starts the watchdog"
check_contains "$BASE/magisk/script/libuperf.sh" "^    uperf_watchdog_stop$" "uperf_stop stops the watchdog"
check_contains "$BASE/magisk/uninstall.sh" "uperf_watchdog.lock" "the uninstaller stops it by pid"

# ---------------------------------------------------------------- summary

echo
echo "== watchdog host harness: $PASS passed, $FAIL failed"
[ "$FAIL" = "0" ] || exit 1
exit 0
