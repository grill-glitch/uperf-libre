SKIPUNZIP=0

# Seed dfps.txt BEFORE setup.sh runs.
#
# Order matters: setup.sh -> install_uperf() does `rm -rf $MODULE_PATH/config`
# after copying the matched SoC config out, so `config/dfps.default.txt` is gone
# by the time setup.sh returns. Seeding after it would silently never fire, and
# a fresh install would boot with no dfps.txt (dfps logs "Rust: dfps disabled"
# and the refresh-rate tab shows nothing).
#
# Only on a fresh install: an existing dfps.txt belongs to the user. That also
# covers someone migrating from the separate yc_dfps module — their own rules
# stay put, which AGENT.md §12.2 accepts as the cost of the embedded mode.
USER_PATH_DFPS="/sdcard/Android/yc/uperf"
mkdir -p "$USER_PATH_DFPS" 2>/dev/null
if [ ! -e "$USER_PATH_DFPS/dfps.txt" ] && [ -f "$MODPATH/config/dfps.default.txt" ]; then
    cp -f "$MODPATH/config/dfps.default.txt" "$USER_PATH_DFPS/dfps.txt"
    chmod 644 "$USER_PATH_DFPS/dfps.txt"
fi

sh $MODPATH/script/setup.sh
[ "$?" != "0" ] && abort
