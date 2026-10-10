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

# ------------------------------------------------------- case 6: syntax + wiring

echo "== case 6: scripts parse and the module wiring is present"
for s in "$WATCHDOG" "$BASE/magisk/script/libuperf.sh" "$BASE/magisk/uninstall.sh" "$BASE/magisk/script/webui.sh"; do
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
