# Shell dependency inventory: what the scripts expect from the ROM

The Rust daemon has no runtime dependencies beyond libc. The shell side does: 48 distinct
external commands across `magisk/*.sh` and `magisk/script/*.sh`. This file is the list,
generated from the scripts themselves (every command position: start of line, after
`;`/`&&`/`||`/`|`/`(`, and inside `$(…)` or backticks), not from memory.

The module used to ship BusyBox 1.38.0 in `magisk/bin/busybox/` and prepend it to `PATH`.
That is gone: it cost 787 KB of the 1.9 MB zip, it shadowed the ROM's own applets (once
making `ps` report a running daemon as `daemon.count=0` — `docs/records/m7-evidence.md`
§7.3), and on the devices this module actually runs on it covered nothing: every command
below resolves to `/system/bin` already. The scripts now use the ROM's toybox directly.

## A. Commands the scripts call (37)

| command | called from |
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

## B. Android platform tools (11)

Not toolbox programs: these are AOSP shell commands. They are called without a path, so
they must exist on the device; all were confirmed present on the test device.

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

## C. What floor this implies

Verified on **alioth, crDroid 16, toybox 0.8.12**: all 37 in A are present in `/system/bin`,
and the module runs with `magisk/bin/busybox` deleted outright — `webui.sh restart`
returned `restart.ok=1`, the daemon came back, the watchdog came back
(`state=running detail=supervisor + worker up`) and the governor returned to `schedutil`.

Below that floor the failure mode is **silent degradation, not an error**: the command is
simply absent, `sh` prints `not found` to a log nobody reads, and the tuning step it
belonged to quietly does nothing. The commands at risk on older toybox, and what stops
working with each:

| command | used by | if missing |
|---|---|---|
| `chrt`, `taskset`, `renice` | `libcgroup.sh` | RT priority / CPU affinity / nice are not applied — the thread just keeps whatever it had |
| `ps -Ao pid,args` | `libcgroup.sh` | kswapd scan finds nothing, so the "don't preempt a reclaiming device" guard never triggers |
| `chattr ±i` | `miui_migt.sh` | files that should be immutable stay writable |
| `sha256sum` | `webui.sh` | WebUI asset verification silently passes |
| `seq` | `platform_special.sh`, `powercfg_once.sh` | loop bodies iterate zero times |
| `usleep 500` | `libpowercfg.sh` | the 500 µs settle after stopping perf-hal is skipped |
| `awk` | `libcgroup.sh`, `libuperf.sh`, `uperf_watchdog.sh`, `webui.sh` | the widest blast radius: `awk` arrived in toybox late, and it parses `/proc` and state files |

Because that is invisible at runtime, `initsvc.sh` now reports it once at boot:
`uperf_check_shell_deps()` walks this list, and any missing command is written to
`/sdcard/Android/yc/uperf/initsvc.log` as
`uperf: missing shell command(s): … -- ROM toybox too old?`. That is the only place the
module will tell you the platform it is on is below the supported floor.

## D. Why `ps` is still spelled `/system/bin/ps`

`webui.sh` sets `SYS_PS=/system/bin/ps` and falls back to `ps` only if that is not
executable. The busybox `ps` that made this necessary is gone, but the explicit path stays:
status output must not depend on whatever `PATH` resolves to, now or after some future
toolbox decision. `libcgroup.sh` keeps calling plain `ps -Ao pid,args` because it wants
whichever `ps` the ROM provides.
