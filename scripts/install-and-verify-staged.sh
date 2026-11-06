#!/system/bin/sh
# Install the freshly built module and verify the staged copy *before* the boot.
# `ksud module install` stages into /data/adb/modules_update/<id>; it becomes active on
# the next boot. The 2026-10-11 incident left same-size all-NUL stubs after a panic, so
# every staged file is checked against the zip's manifest here, and again after boot.
set -u
ZIP=/data/local/tmp/uperf-magisk.zip
STAGE=/data/adb/modules_update/uperf
MAN=/data/local/tmp/uperf-zip-manifest.txt

echo "== install =="
ksud module install $ZIP 2>&1 | tail -5
echo "== staged tree =="
ls -d $STAGE 2>/dev/null || { echo "FAIL: nothing staged"; exit 1; }
n=$(find $STAGE -type f | wc -l)
echo "staged files: $n (zip has $(wc -l < $MAN))"
echo "staged bin md5: $(md5sum $STAGE/bin/uperf 2>/dev/null)"

echo "== manifest compare (staged) =="
bad=0
while read -r want path; do
    [ -n "${path:-}" ] || continue
    got=$(md5sum "$STAGE/$path" 2>/dev/null | cut -d' ' -f1)
    if [ "$got" != "$want" ]; then
        echo "MISMATCH $path want=$want got=${got:-missing}"
        bad=$((bad + 1))
    fi
done < $MAN
echo "mismatches: $bad"

echo "== NUL check (size-preserved garbage) =="
nul=0
for f in $(find $STAGE -type f); do
    if [ -s "$f" ] && [ "$(tr -d '\000' < "$f" | wc -c)" = "0" ]; then
        echo "ALL-NUL $f"
        nul=$((nul + 1))
    fi
done
echo "all-nul files: $nul"

echo "== module.prop of the staged copy =="
head -3 $STAGE/module.prop 2>/dev/null

sync
echo "SYNCED"
[ "$bad" = "0" ] && [ "$nul" = "0" ] || exit 1
