#!/system/bin/sh
#
# Post-install / post-boot verification of the M9 wiring (runs ON the device).
#
# The pre-install harness (`scripts/m9-device-verify.sh`) proves the policy against a
# fake sysfs tree, but two things only exist once the module is actually installed and
# the device has started with it:
#
#   1. that KernelSU's/Magisk's `service.sh` path really starts the watchdog, and that
#      it is still there and still sampling after boot (the `setsid` + no-supervisor
#      property);
#   2. that a WebUI-style restart (`webui.sh restart` -> `uperf_stop` + `uperf_start`)
#      hands the owner lock over cleanly: the old watchdog gone, a new one owning the
#      lock, the daemon back up.
#
# Driver (host side):
#   adb -s <serial> push scripts/m9-device-boot-verify.sh /data/local/tmp/
#   adb -s <serial> shell su -c 'sh /data/local/tmp/m9-device-boot-verify.sh'
#
# Read-only except for the restart in part 3, which is the module's own control path.

T=/data/adb/modules/uperf
U=/sdcard/Android/yc/uperf
BIN="$T/bin/uperf"
LOCK="$T/flag/uperf_watchdog.lock"
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
check() {
    if [ "$2" = "0" ]; then ok "$1"; else bad "$1"; fi
}
eq() {
    if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (got '$1', want '$2')"; fi
}
state_of() { sed -n "s/^$2=//p" "$1" 2>/dev/null | head -n 1; }
wait_for() { # wait_for <file> <key> <value> <seconds>
    local i=0
    while [ "$i" -lt "$4" ]; do
        [ "$(state_of "$1" "$2")" = "$3" ] && return 0
        sleep 1
        i=$((i + 1))
    done
    return 1
}
alive() { kill -0 "$1" 2>/dev/null; }
has_line() {
    if grep -q "$2" "$1" 2>/dev/null; then ok "$3"; else bad "$3"; fi
}
# The watchdog log survives restarts (it is only trimmed at 128 KiB), so "has it
# complained this boot" must look at the tail after its last start line, not the
# whole file.
since_last_start() {
    awk '/watchdog started/ { n = NR } { l[NR] = $0 } END { for (i = n; i <= NR; i++) print l[i] }' "$1" 2>/dev/null
}
now_ms() { awk '{printf "%d", $1 * 1000}' /proc/uptime; }

echo "== M9 boot verification: $(getprop ro.product.device), kernel $(uname -r), uptime $(awk '{printf "%d", $1}' /proc/uptime) s"
BOOT_ID="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
echo "   boot_id=$BOOT_ID"

# ------------------------------------------------- part 1: the module is the new build

echo
echo "== part 1: the installed module carries the M9 files"
[ -f "$T/script/uperf_watchdog.sh" ] && c=0 || c=1
check "the watchdog script is installed" "$c"
[ -f "$T/bin/uperf" ] && c=0 || c=1
check "the daemon binary is installed" "$c"
echo "   binary: $(md5sum "$BIN" 2>/dev/null)"
echo "   watchdog: $(md5sum "$T/script/uperf_watchdog.sh" 2>/dev/null)"

# ------------------------------------------------- part 2: the daemon AND the watchdog

echo
echo "== part 2: the daemon and the watchdog both came up from service.sh"
SUP=""
WORK=""
for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2 == "uperf" { print $1 }'); do
    exe="$(readlink "/proc/$p/exe" 2>/dev/null)"
    [ "$exe" = "$BIN" ] || continue
    line=""
    IFS= read -r line <"/proc/$p/stat" 2>/dev/null
    # shellcheck disable=SC2086
    set -- ${line##*) }
    [ "$2" = "1" ] && SUP="$p" || WORK="$p"
done
echo "   daemon: supervisor=[$SUP] worker=[$WORK]"
[ -n "$SUP" ] && [ -n "$WORK" ] && c=0 || c=1
check "the daemon is running (supervisor + worker)" "$c"

# The installed daemon must be reporting status: that is `status.rs` in the shipped
# build, and its boot_id proves the file is from *this* boot.
eq "$(state_of "$U/uperf.state" boot_id)" "$BOOT_ID" "uperf.state is from this boot"
eq "$(state_of "$U/uperf.state" state)" "running" "uperf.state says running"
echo "   takeover=$(state_of "$U/uperf.state" takeover) armed=$(state_of "$U/uperf.state" armed)"

WD_PID="$(cut -d: -f1 "$LOCK/owner" 2>/dev/null)"
WD_START="$(cut -d: -f3 "$LOCK/owner" 2>/dev/null)"
echo "   watchdog: pid=$WD_PID start_ticks=$WD_START owner=$(cat "$LOCK/owner" 2>/dev/null)"
[ -n "$WD_PID" ] && alive "$WD_PID" && c=0 || c=1
check "a watchdog owns the lock and is alive (pid ${WD_PID:-?})" "$c"
if [ -n "$WD_PID" ]; then
    cmd="$(tr '\0' ' ' <"/proc/$WD_PID/cmdline" 2>/dev/null)"
    case "$cmd" in
    *uperf_watchdog.sh*) ok "that pid really runs the watchdog ([$cmd])" ;;
    *) bad "that pid really runs the watchdog (cmdline=[$cmd])" ;;
    esac
    # start_ticks in the lock must match the live process (PID-reuse defence)
    line=""
    IFS= read -r line <"/proc/$WD_PID/stat" 2>/dev/null
    # shellcheck disable=SC2086
    set -- ${line##*) }
    eq "${20}" "$WD_START" "the lock's start_ticks match the live process"
fi

# The watchdog writes `starting` and only `running` after its first healthy sample, so
# a run that begins seconds after a restart must not be judged mid-bring-up.
wait_for "$U/uperf_watchdog.state" state running 20

# This is the property `setsid` exists for: it outlived the shell service.sh ran it
# from, and it is *sampling* — a stale file from the previous boot would have the old
# boot_id (checked above) or an old updated_uptime_ms (checked here).
eq "$(state_of "$U/uperf_watchdog.state" boot_id)" "$BOOT_ID" "uperf_watchdog.state is from this boot"
eq "$(state_of "$U/uperf_watchdog.state" state)" "running" "the watchdog calls the daemon healthy"
upd="$(state_of "$U/uperf_watchdog.state" updated_uptime_ms)"
now="$(now_ms)"
# 60 s: two intervals plus slack (the state file is rewritten every sample, so a
# stale timestamp means it stopped sampling — not that it had nothing to say).
if [ -n "$upd" ] && [ -n "$now" ] && [ "$((now - upd))" -lt 60000 ]; then
    ok "it is still sampling (last write ${upd} ms, now ${now} ms, $(( (now - upd) / 1000 )) s ago)"
else
    bad "it is still sampling (last write ${upd:-?} ms, now ${now:-?} ms)"
fi
has_line "$U/uperf_watchdog.log" "watchdog started" "the watchdog logged its start this boot"
since_last_start "$U/uperf_watchdog.log" | grep -q "unhealthy" 2>/dev/null && c=1 || c=0
check "no unhealthy sample since it started" "$c"

# ------------------------------------------------- part 3: the restart handover

echo
echo "== part 3: a WebUI-style restart hands the lock over"
OLD_WD="$WD_PID"
sh "$T/script/webui.sh" restart >"/data/local/tmp/m9-restart.out" 2>&1
rc=$?
eq "$rc" "0" "webui.sh restart exits 0"
sleep 8
NEW_SUP=""
NEW_WORK=""
for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2 == "uperf" { print $1 }'); do
    [ "$(readlink "/proc/$p/exe" 2>/dev/null)" = "$BIN" ] || continue
    line=""
    { IFS= read -r line <"/proc/$p/stat"; } 2>/dev/null
    # shellcheck disable=SC2086
    set -- ${line##*) }
    [ "$2" = "1" ] && NEW_SUP="$NEW_SUP $p" || NEW_WORK="$NEW_WORK $p"
done
echo "   daemon now: supervisor=[${NEW_SUP# }] worker=[${NEW_WORK# }]"
[ -n "$NEW_WORK" ] && c=0 || c=1
check "the daemon is back up after the restart" "$c"
# Deterministic form of the failure this found: the *previous* worker's stop write can
# land after the new worker claimed the status file (its `uperf_rs_stop()` may join for
# up to 2 s while `uperf_start` has already moved on). Asserting on the file's owner
# catches the race whatever the timing, unlike "state=running" alone.
eq "$(state_of "$U/uperf.state" pid)" "${NEW_WORK# }" "uperf.state is owned by the live worker"
eq "$(state_of "$U/uperf.state" state)" "running" "and still says running (no stale stop landed)"
NEW_WD="$(cut -d: -f1 "$LOCK/owner" 2>/dev/null)"
echo "   watchdog: old=$OLD_WD new=$NEW_WD"
[ -n "$NEW_WD" ] && alive "$NEW_WD" && c=0 || c=1
check "a watchdog owns the lock again (pid ${NEW_WD:-?})" "$c"
[ "$NEW_WD" != "$OLD_WD" ] && c=0 || c=1
check "it is a *new* watchdog, not the old one" "$c"
if [ -n "$OLD_WD" ]; then
    alive "$OLD_WD" && c=1 || c=0
    check "the old watchdog is gone (no double supervision)" "$c"
fi
eq "$(state_of "$U/uperf_watchdog.state" state)" "running" "the new watchdog reports the daemon healthy"
echo "   restart output: $(tr '\n' ' ' <"/data/local/tmp/m9-restart.out" | cut -c1-200)"

# ------------------------------------------------- part 4: log bounds

echo
echo "== part 4: the log is bounded"
for f in "$U/uperf_log.txt" "$U/uperf_log.1.txt" "$U/uperf_log.2.txt" "$U/uperf_log.txt.bak"; do
    [ -f "$f" ] && echo "   $(basename "$f"): $(stat -c %s "$f") bytes"
done
sz="$(stat -c %s "$U/uperf_log.txt" 2>/dev/null)"
[ -n "$sz" ] && [ "$sz" -le 8388608 ] && c=0 || c=1
check "the current log is under 8 MiB (${sz:-?} bytes)" "$c"
baksz="$(stat -c %s "$U/uperf_log.txt.bak" 2>/dev/null)"
if [ -n "$baksz" ]; then
    [ "$baksz" -le 16777216 ] && c=0 || c=1
    check "the backup is under the script's cap (${baksz} bytes)" "$c"
fi

echo
echo "== M9 boot verification: $PASS passed, $FAIL failed"
echo "completed=$(date '+%H:%M:%S') passed=$PASS failed=$FAIL" >/data/local/tmp/m9-boot-completed
[ "$FAIL" = "0" ] || exit 1
exit 0
