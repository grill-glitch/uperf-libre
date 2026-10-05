SKIPUNZIP=0
sh $MODPATH/script/setup.sh
[ "$?" != "0" ] && abort

# Seed dfps.txt on fresh installs so dfps_task can load the rule table at
# boot. T07: customize.sh is the only place we ship a default; users edit
# their own copy under /sdcard/Android/yc/uperf/dfps.txt afterwards. If the
# user already has a dfps.txt (e.g. migrating from a previous install of the
# the vendored yc_dfps module), leave it alone.
USER_PATH_DFPS="/sdcard/Android/yc/uperf"
mkdir -p "$USER_PATH_DFPS" 2>/dev/null
if [ ! -e "$USER_PATH_DFPS/dfps.txt" ] && [ -f "$MODPATH/config/dfps.default.txt" ]; then
    cp -f "$MODPATH/config/dfps.default.txt" "$USER_PATH_DFPS/dfps.txt"
    chmod 644 "$USER_PATH_DFPS/dfps.txt"
fi
