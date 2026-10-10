#!/system/bin/sh
#
# Is `scaling_max_freq` actually writable on this kernel?
#
# This decides whether the `userspace` takeover is *necessary*, which is the premise
# the whole frequency design rests on: `docs/m7-evidence.md` §6 recorded EACCES for a
# different value on alioth, and the module's own `powercfg_once.sh` leaves the node at
# mode 0444. Run it again after a kernel or ROM change (`sh <this> [policy]`).
#
# Safety: it writes a *lower* cap on the little cluster (never higher), restores the
# original immediately, and a trap restores it even if the script dies. A rejected
# write changes nothing.
#
# Driver (host side):
#   adb -s <serial> push scripts/cpufreq-write-probe.sh /data/local/tmp/
#   adb -s <serial> shell su -c 'sh /data/local/tmp/cpufreq-write-probe.sh policy0'

P="${1:-policy0}"
D="/sys/devices/system/cpu/cpufreq/$P"
orig="$(cat "$D/scaling_max_freq" 2>/dev/null)"
min="$(cat "$D/scaling_min_freq" 2>/dev/null)"
mode="$(stat -c %a "$D/scaling_max_freq" 2>/dev/null)"
target=$((orig - 100000))

# Only a write that *landed* needs undoing. Writing a refused value changes nothing, so
# an unconditional restore just prints another refusal on the way out.
changed=no
restore() {
    [ "$changed" = yes ] || return 0
    echo "$orig" >"$D/scaling_max_freq" 2>/dev/null
    changed=no
}
trap 'restore' EXIT INT TERM

echo "== $P on $(getprop ro.product.device), kernel $(uname -r)"
echo "   scaling_max_freq=$orig (mode $mode) scaling_min_freq=$min scaling_governor=$(cat "$D/scaling_governor" 2>/dev/null)"
if [ "$target" -le "$min" ]; then
    echo "   !! target $target <= min $min; refusing"
    exit 1
fi

# Neither the readback nor `2>&1` on the command alone is enough: writing the value
# that is already there leaves the readback identical whether it was accepted or
# refused, and the shell reports a *failed redirection* itself, so `2>&1` attached to
# `echo` misses it (both cost this probe a wrong "accepted"/"SUCCEEDED"). The group
# redirect is what captures it.
try_write() { # try_write <value> -> "accepted" | "rejected [why]"
    local err
    err="$( { echo "$1" >"$D/scaling_max_freq"; } 2>&1 )"
    if [ -z "$err" ]; then
        echo "accepted (now $(cat "$D/scaling_max_freq" 2>/dev/null))"
    else
        echo "rejected [$(echo "$err" | sed 's/.*: //')]"
    fi
}

printf '   write back the current value : '
same="$(try_write "$orig")"
echo "$same"
printf '   write a different value (%s) : ' "$target"
diff="$(try_write "$target")"
echo "$diff"
diff_verdict="${diff%% *}"
case "$same $diff" in
*accepted*) changed=yes ;;
esac

restore
after="$(cat "$D/scaling_max_freq" 2>/dev/null)"
echo "   restore: $after (want $orig) $([ "$after" = "$orig" ] && echo OK || echo MISMATCH)"
echo "   cur_freq=$(cat "$D/scaling_cur_freq" 2>/dev/null) governor=$(cat "$D/scaling_governor" 2>/dev/null)"

if [ "$diff_verdict" = rejected ]; then
    echo "   VERDICT: scaling_max_freq is driver read-only here -> the userspace"
    echo "            takeover (scaling_governor=userspace + scaling_setspeed) is the"
    echo "            only frequency path, as docs/m7-evidence.md §6 recorded."
else
    echo "   VERDICT: scaling_max_freq ACCEPTS a different value here -> the takeover is"
    echo "            NOT the only path; a min/max writer would work too (revisit the"
    echo "            'takeover is the only option' premise in docs/m7-evidence.md §6)."
fi
echo PROBE_DONE
