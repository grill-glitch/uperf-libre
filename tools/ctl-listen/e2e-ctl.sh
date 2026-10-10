#!/system/bin/sh
# ⑦ control-plane e2e: the daemon connects OUT to the peer's local socket, does the
# token/version/pid handshake, and keeps the link alive with PING/PONG — then the same
# against a peer that refuses. Teardown removes only uperf pids that were not running.
T=/data/local/tmp/ctl-e2e
OUT=$T/out.log
PS=/system/bin/ps
CL=/data/local/tmp/ctl-listen
say() { echo "$@" >> "$OUT"; }

BEFORE=$($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')
rm -rf "$T"; mkdir -p "$T/user" "$T/root"; : > "$OUT"
say "installed uperf before: $BEFORE"

cp /sdcard/Android/yc/uperf/uperf.json "$T/user/uperf.json" 2>/dev/null
[ -s "$T/user/uperf.json" ] || echo '{}' > "$T/user/uperf.json"
printf 'balance' > "$T/user/cur_powermode.txt"
cp /data/local/tmp/uperf_ctl "$T/uperf_test" && chmod 755 "$T/uperf_test" || say "!! copy failed"
chmod 755 "$CL" 2>/dev/null

run_case() {
    reject="$1"
    tag="$2"
    rm -f "$T/user/uperf.token" "$T/user/ctl_$tag.log" "$T/user/daemon_$tag.log"
    if [ "$reject" = "1" ]; then
        "$CL" @uperf-e2e --reject --seconds 14 >"$T/user/ctl_$tag.log" 2>&1 &
    else
        "$CL" @uperf-e2e --seconds 14 >"$T/user/ctl_$tag.log" 2>&1 &
    fi
    LPID=$!
    sleep 1
    UPERF_FAKE_ROOT="$T/root" \
    UPERF_SCHED_DRY_RUN=1 \
    UPERF_STATE_FILE="$T/user/orig_governor.txt" \
    UPERF_STATUS_FILE="$T/user/uperf.state" \
    UPERF_STATUS_CONFIG="$T/user/uperf.json" \
    UPERF_CTL_SOCKET=@uperf-e2e \
    UPERF_CTL_RETRY_MS=2000 \
    UPERF_CTL_PING_MS=1500 \
        "$T/uperf_test" "$T/user/uperf.json" -o "$T/user/daemon_$tag.log" </dev/null >/dev/null 2>&1 &
    sleep 6
    say "--- peer [$tag] (reject=$reject) ---"
    head -6 "$T/user/ctl_$tag.log" >>"$OUT"
    say "--- daemon [$tag] ---"
    grep -E "ctl-socket" "$T/user/daemon_$tag.log" 2>/dev/null | head -6 >>"$OUT"
    for p in $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}'); do
        case " $BEFORE " in
        *" $p "*) ;;
        *) kill -TERM "$p" 2>/dev/null ;;
        esac
    done
    kill "$LPID" 2>/dev/null
    sleep 2
}

run_case 0 accept
run_case 1 reject

say "token file: $(ls -l "$T/user/uperf.token" 2>/dev/null | awk '{print $1, $5}') bytes"
say "after teardown: $($PS -A -o PID,NAME 2>/dev/null | awk '$2=="uperf"{print $1}' | tr '\n' ' ')"
cat "$OUT"
