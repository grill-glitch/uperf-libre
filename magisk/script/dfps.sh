#!/system/bin/sh
#
# Copyright (C) 2021-2022 Matt Yang
# Copyright (C) 2026 grill-glitch
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# Control entry for the dfps tab in the KernelSU WebUI (and for adb, which
# is how it is tested). Mirrors the `webui.sh` `key=value` protocol so the
# JS frontend can reuse `ctl.js`'s generic runScript helper.
#
# Subcommands (one per WebUI action):
#   status         — print cur=<Hz> + daemon identity, used by the live card
#   info           — print dfps.txt line count + rule map
#   set-rule PKG IDLE ACTIVE
#                  — write or replace one rule line in dfps.txt
#                    (dfps_task reloads on close_write via watch_task)
#   restart        — restart uperf daemon via libuperf.sh:uperf_restart
#
# Output is `key=value` lines, one per line, parsable on the JS side via
# `key.split('=')` (the same convention webui.sh uses).

BASEDIR="$(cd "$(dirname "$0")" && pwd)"
. "$BASEDIR/pathinfo.sh"
. "$BASEDIR/libcommon.sh"
. "$BASEDIR/libuperf.sh"

DFPS_CONFIG="$USER_PATH/dfps.txt"
DFPS_CUR="$USER_PATH/dfps_cur.txt"

print_status() {
    local cur="(none)"
    if [ -f "$DFPS_CUR" ]; then
        # First line only: stripping *all* whitespace concatenated a momentarily multi-line
        # file into e.g. "60120". Anything non-numeric is reported as invalid.
        cur="$(head -n 1 "$DFPS_CUR" 2>/dev/null | tr -d '[:space:]')"
        case "$cur" in
            '') cur="(empty)" ;;
            *[!0-9]*) cur="(invalid)" ;;
        esac
    fi
    echo "cur=${cur}"
    echo "config.path=$DFPS_CONFIG"
    echo "config.exists=$([ -f "$DFPS_CONFIG" ] && echo 1 || echo 0)"
}

print_info() {
    local n=0 rules=""
    if [ -f "$DFPS_CONFIG" ]; then
        # `-E`, not `\|`: Android's grep is toybox, whose BRE parser has no
        # alternation, so `'$ \| ^# \| ^/'` matches nothing and `-v` filters
        # nothing out — the rule list came back with the comments and the
        # `/tunable` lines included (caught on the device: 7 lines instead of
        # 3). ERE is what toybox implements.
        # `grep -c` prints 0 *and* exits 1 when nothing matches: `|| echo 0` appended a
        # second 0 ("0\n0") and broke the key=value protocol. Default only on empty output.
        n=$(grep -c -v -E '^[[:space:]]*$|^#|^/' "$DFPS_CONFIG" 2>/dev/null)
        [ -n "$n" ] || n=0
        rules="$(grep -v -E '^[[:space:]]*$|^#|^/' "$DFPS_CONFIG" 2>/dev/null | sed 's/[[:space:]]\+/ /g' | tr '\n' '|')"
    fi
    echo "lines=$n"
    echo "rules=$rules"
}

# `uperf_pid_count` lives in webui.sh, which is a separate entry point — using it here made
# every count print "not found" and `restart.ok` report failure for a working restart.
_uperf_pid_count() {
    ps -A -o PID,STAT,NAME 2>/dev/null | awk '$NF == "uperf" && $2 !~ /Z/ { n++ } END { print n + 0 }'
}

# Replaces one rule line in dfps.txt. PKG may be "*" (universal) or "-"
# (offscreen) or a real package name. IDLE and ACTIVE are integers.
#
# Safety: write to a temp file in the same dir, fsync, then rename. dfps_task
# watches with inotify CLOSE_WRITE and reloads the new contents in <100ms
# (M2 acceptance: a `cat dfps.txt && cat dfps.txt` test shows two reload
# events back-to-back).
set_rule() {
    local pkg="$1" idle="$2" active="$3"
    if [ -z "$pkg" ] || [ -z "$idle" ] || [ -z "$active" ]; then
        echo "rule.pkg="
        echo "rule.idle="
        echo "rule.active="
        echo "rule.ok=0"
        echo "rule.error=missing argument (need: pkg idle active)"
        return 1
    fi
    # Validate each field separately and bound it: concatenating the two used to accept
    # `idle=1-`, `active=--` and `idle=-60`, which the daemon's own rule validation then
    # rejects — the edit silently never took effect while `rule.ok=1` was reported.
    case "$pkg" in
        '' | *[!A-Za-z0-9._:*-]*)
            echo "rule.ok=0"; echo "rule.error=invalid package name"; return 1 ;;
    esac
    case "$idle" in '' | *[!0-9]*) echo "rule.ok=0"; echo "rule.error=idle must be a non-negative integer"; return 1 ;; esac
    case "$active" in '' | *[!0-9]*) echo "rule.ok=0"; echo "rule.error=active must be a non-negative integer"; return 1 ;; esac
    if [ "$idle" -gt 240 ] || [ "$active" -gt 240 ]; then
        echo "rule.ok=0"; echo "rule.error=hz out of range (0..240)"; return 1
    fi
    mkdir -p "$USER_PATH" 2>/dev/null
    if [ ! -f "$DFPS_CONFIG" ]; then
        # Both special rules are required upstream (dfps_config::ParseError).
        # Seed a minimal-but-valid config: every pkg line appends below; the
        # default install ships dfps.default.txt (T07) so this branch is the
        # truly-zero-config path.
        echo "* 0 60" > "$DFPS_CONFIG"
        echo "- 0 30" >> "$DFPS_CONFIG"
    fi
    # Field-exact matching, not a grep pattern: `pkg='*'` is a valid rule name but an invalid
    # BRE (`^* `), and `pkg='.*'` would match every line and delete the whole table.
    local tmp
    tmp="$(mktemp "${DFPS_CONFIG}.XXXXXX" 2>/dev/null)" || tmp="${DFPS_CONFIG}.$$.tmp"
    if awk -v p="$pkg" '$1 == p { found = 1 } END { exit !found }' "$DFPS_CONFIG" 2>/dev/null; then
        awk -v p="$pkg" '$1 != p' "$DFPS_CONFIG" > "$tmp" || { rm -f "$tmp"; echo "rule.ok=0"; echo "rule.error=read failed"; return 1; }
    else
        cp -f "$DFPS_CONFIG" "$tmp" || { rm -f "$tmp"; echo "rule.ok=0"; echo "rule.error=copy failed"; return 1; }
    fi
    printf '%s %s %s\n' "$pkg" "$idle" "$active" >> "$tmp" || { rm -f "$tmp"; echo "rule.ok=0"; echo "rule.error=write failed"; return 1; }
    chmod 644 "$tmp" 2>/dev/null
    sync
    mv -f "$tmp" "$DFPS_CONFIG" || { rm -f "$tmp"; echo "rule.ok=0"; echo "rule.error=rename failed"; return 1; }
    # Read back to verify.
    local got
    got="$(awk -v p="$pkg" '$1 == p { print; exit }' "$DFPS_CONFIG" 2>/dev/null | sed 's/[[:space:]]\+/ /g')"
    echo "rule.pkg=$pkg"
    echo "rule.idle=$idle"
    echo "rule.active=$active"
    echo "rule.got=$got"
    if [ "$got" = "$pkg $idle $active" ]; then
        echo "rule.ok=1"
    else
        echo "rule.ok=0"
        echo "rule.error=read-back mismatch"
        return 1
    fi
}

do_restart() {
    echo "restart.before=$(_uperf_pid_count)"
    uperf_stop
    uperf_start
    local n; n="$(_uperf_pid_count)"
    echo "restart.after=$n"
    [ "$n" -gt 0 ] && echo "restart.ok=1" || echo "restart.ok=0"
}

case "$1" in
    status) print_status ;;
    info) print_info ;;
    set-rule) shift; set_rule "$@" ;;
    restart) do_restart ;;
    *)
        echo "Usage: dfps.sh {status|info|set-rule <pkg> <idle> <active>|restart}" >&2
        exit 2
        ;;
esac