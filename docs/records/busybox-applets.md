# Dependency inventory: what the shell side actually needs

The Rust daemon has no runtime dependencies beyond libc. The shell side does: 48 distinct
external commands across `magisk/*.sh` and `magisk/script/*.sh`. This file is the list,
generated from the scripts themselves (every command position: start of line, after
`;`/`&&`/`||`/`|`/`(`, and inside `$(…)` or backticks), not from memory.

Because `pathinfo.sh:27` puts `magisk/bin/busybox` at the front of `PATH`, each of these
names resolves to **busybox first** and to `/system/bin` only if busybox lacks the applet.
So busybox is the arbiter for every row below.

## A. Resolved by busybox (37 applets)

| applet | called from |
|---|---|
| `awk` | `script/libcgroup.sh`, `script/libuperf.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `basename` | `script/libuperf.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `cat` | `script/dfps.sh`, `script/libcgroup.sh`, `script/libsysinfo.sh`, `script/libuperf.sh`, `script/miui_migt.sh`, `script/platform_special.sh`, `script/powercfg_once.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `chattr` | `script/miui_migt.sh` |
| `chmod` | `customize.sh`, `script/dfps.sh`, `script/initsvc.sh`, `script/libcommon.sh`, `script/miui_migt.sh`, `script/powercfg_once.sh`, `script/setup.sh`, `script/uperf_watchdog.sh` |
| `chown` | `script/libcommon.sh`, `script/setup.sh`, `script/uperf_watchdog.sh` |
| `chrt` | `script/libcgroup.sh` |
| `cp` | `customize.sh`, `script/dfps.sh`, `script/initsvc.sh`, `script/libuperf.sh`, `script/miui_migt.sh`, `script/setup.sh`, `uninstall.sh` |
| `cut` | `customize.sh`, `script/libcommon.sh`, `script/libpowercfg.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `script/webui.sh`, `uninstall.sh` |
| `date` | `script/uperf_watchdog.sh`, `script/webui.sh` |
| `dirname` | `script/dfps.sh`, `script/initsvc.sh`, `script/libuperf.sh`, `script/miui_migt.sh`, `script/pathinfo.sh`, `script/platform_special.sh`, `script/powercfg_main.sh`, `script/powercfg_once.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `script/webui.sh`, `service.sh` |
| `find` | `script/setup.sh` |
| `grep` | `script/dfps.sh`, `script/libcgroup.sh`, `script/libcommon.sh`, `script/libpowercfg.sh`, `script/libsysinfo.sh`, `script/libuperf.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `head` | `script/dfps.sh`, `script/libcommon.sh`, `script/libpowercfg.sh`, `script/libsysinfo.sh`, `script/webui.sh` |
| `kill` | `script/libuperf.sh`, `script/uperf_watchdog.sh`, `uninstall.sh` |
| `killall` | `script/libuperf.sh`, `script/powercfg_once.sh`, `service.sh`, `uninstall.sh` |
| `ls` | `customize.sh`, `script/libcgroup.sh`, `script/libuperf.sh`, `script/platform_special.sh`, `script/setup.sh` |
| `mkdir` | `customize.sh`, `script/dfps.sh`, `script/libuperf.sh`, `script/miui_migt.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `script/webui.sh`, `uninstall.sh` |
| `mv` | `script/dfps.sh`, `script/libuperf.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `uninstall.sh` |
| `ps` | `script/libcgroup.sh` (`ps -Ao pid,args` — needs `CONFIG_DESKTOP`) |
| `readlink` | `script/initsvc.sh`, `script/miui_migt.sh`, `script/pathinfo.sh`, `script/platform_special.sh`, `script/powercfg_main.sh`, `script/powercfg_once.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `service.sh` |
| `renice` | `script/libcgroup.sh` |
| `rm` | `post-fs-data.sh`, `script/libcommon.sh`, `script/libuperf.sh`, `script/miui_migt.sh`, `script/setup.sh`, `script/uperf_watchdog.sh`, `service.sh`, `uninstall.sh` |
| `sed` | `script/dfps.sh`, `script/libcommon.sh` |
| `seq` | `script/platform_special.sh`, `script/powercfg_once.sh` |
| `sh` | `customize.sh`, `script/initsvc.sh`, `service.sh` — the applet's own `ash`, since `FEATURE_SH_STANDALONE` is on |
| `sha256sum` | `script/webui.sh` |
| `sleep` | `script/libcommon.sh`, `script/libuperf.sh`, `script/uperf_watchdog.sh`, `service.sh`, `uninstall.sh` |
| `sort` | `script/libsysinfo.sh` |
| `tail` | `script/uperf_watchdog.sh`, `script/webui.sh` |
| `taskset` | `script/libcgroup.sh` |
| `touch` | `script/libcommon.sh`, `script/setup.sh` |
| `tr` | `script/dfps.sh`, `script/libcommon.sh`, `script/libsysinfo.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `uname` | `script/webui.sh` |
| `usleep` | `script/libpowercfg.sh` (`usleep 500`) |
| `wc` | `script/libsysinfo.sh`, `script/libuperf.sh`, `script/uperf_watchdog.sh`, `script/webui.sh` |
| `which` | `script/powercfg_once.sh` |

## B. Resolved by the ROM (11 commands)

These are Android shell tools, not toolbox programs. They are called without a path, so
they must exist on the device — every one of them is AOSP, and all were confirmed present
on the test device (crDroid 16).

| command | path on device | called from |
|---|---|---|
| `chcon` | `/system/bin/chcon` | `customize.sh`, `script/setup.sh`, `script/uperf_watchdog.sh` |
| `getenforce` | `/system/bin/getenforce` | `script/webui.sh` (display only) |
| `getprop` | `/system/bin/getprop` | `script/libcommon.sh`, `script/libsysinfo.sh`, `script/setup.sh`, `script/webui.sh`, `uninstall.sh` |
| `logcat` | `/system/bin/logcat` | `service.sh` |
| `mount` | `/system/bin/mount` | `script/libcommon.sh` (`--bind` mount masking), `script/miui_migt.sh` |
| `pm` | `/system/bin/pm` | `script/libpowercfg.sh` |
| `rmdir` | `/system/bin/rmdir` | `script/powercfg_once.sh` |
| `settings` | `/system/bin/settings` | `script/miui_migt.sh` |
| `start` | `/system/bin/start` | `script/libpowercfg.sh`, `script/powercfg_once.sh` |
| `stop` | `/system/bin/stop` | `script/libpowercfg.sh`, `script/powercfg_once.sh` |
| `umount` | `/system/bin/umount` | `script/libcommon.sh`, `script/miui_migt.sh` |

(`su`, `ksud`, `magisk` are invoked through the module's own helpers, not from these
scripts' command positions.)

## What this list decides

1. **busybox must contain A, at minimum.** The shipped build has 314 applets, i.e. all of A
   plus the rest of upstream's `defconfig` toolbox; see `busybox-build/README.md` for what is
   in, what is out, and why. Applet *features* matter as much as applet presence —
   `readlink -f` is what broke the watchdog during this work.
2. **A is not strictly required today.** On the test device every applet in A also exists in
   `/system/bin` (toybox), so busybox is not covering a missing tool there — it is pinning
   behaviour across ROMs, which is where old or trimmed toybox would otherwise bite
   (`chrt`, `taskset`, `renice` arrived in toybox only around Android 8–9, while this
   module claims Android 6+; `sha256sum`, `seq`, `usleep`, `chattr` are not in old toolbox
   builds at all).
3. **The one deliberate mismatch is `ps`.** `libcgroup.sh` calls `ps -Ao pid,args`; toybox
   accepts it, busybox needs `CONFIG_DESKTOP`. If `ps` were dropped from the build, the
   call would silently fall through to toybox with different column widths — so `ps` stays
   in and `CONFIG_DESKTOP=y` is part of the config.
