# busybox, and how the shipped binary is made

`magisk/bin/busybox/busybox` is the module's private toolbox. `pathinfo.sh` puts its
directory at the front of `PATH` and `initsvc.sh` runs `busybox --install -s` there, so
every script in `magisk/script/` resolves `awk`, `sed`, `killall`, `sha256sum`, … to this
binary before it resolves them to the ROM's toybox. That is the point: the module then
behaves the same on a ROM whose toybox is old, trimmed, or patched, instead of depending
on which applets the vendor shipped.

It used to be a prebuilt from `osm0sis/android-busybox-ndk` (BusyBox 1.31.1, 356 applets,
2,087,000 B). It is now built from the upstream release by this repository, so the binary's
provenance is a signature, a patch and a script rather than "trust this zip".

## Provenance chain

| step | value |
|---|---|
| upstream | `busybox-1.38.0.tar.bz2` from <https://busybox.net/downloads/> |
| signature | detached `.sig`, Denys Vlasenko, fingerprint `C9E9 416F 76E6 10DB D09D 040F 47B7 0C55 ACC9 965B` (key in `denys-vlasenko-pubkey.asc`) |
| tarball sha256 | `34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2` |
| patches | `uperf-bionic.patch` (4 files, see below) |
| config | `busybox-aarch64-uperf.config` — upstream `defconfig` minus the applets bionic cannot build |
| build | NDK r30 clang, `aarch64-linux-android26-`, `LDFLAGS="-static -no-pie"` |
| result | `ELF 64-bit LSB executable, ARM aarch64, statically linked`, 1,358,320 B, 314 applets |
| sha256 | `e2b7a98a7d8d0aa4e65d19ebbbce7d5e3bbb1a8dca0a7f73bc6579ed3e391dab` |

`build-busybox.sh` performs every one of those steps and is reproducible: two runs on
different days produce the same sha256, because `SOURCE_DATE_EPOCH=1767225600` and
`TZ=Asia/Shanghai` are pinned (busybox embeds `BusyBox v1.38.0 (2026-01-01 08:00:00 CST)`
in the banner, and that string is part of the file). Change either variable and the hash
changes; that is expected.

```
cd docs/records/busybox-build
./build-busybox.sh            # build, print sha256
./build-busybox.sh --install  # build and copy into magisk/bin/busybox/
```

## Why a patch is needed at all

BusyBox 1.38.0 builds for glibc, musl and uClibc; bionic is a fourth case it only partly
handles. `uperf-bionic.patch` is four small, self-documenting hunks:

1. `include/platform.h` — bionic has had `strchrnul()` since API 24, but busybox un-defines
   `HAVE_STRCHRNUL` for every Android build and then defines its own copy in
   `libbb/platform.c`; linking static libc.a then fails with `duplicate symbol: strchrnul`.
   The hunk keeps the busybox copy only below API 24.
2. `libbb/missing_syscalls.c` — same story for `getsid`, `sethostname` and `adjtimex`: the
   file re-implements them via raw syscalls for Android, but bionic has exported them for
   years. Guarded to API < 26.
3. `libbb/yescrypt/{alg-yescrypt-common,alg-yescrypt-kdf}.c` — bionic has no
   `explicit_bzero()`. The hunk defines it as `memset(buf, 0, n)`, which is what the libc
   function compiles to; only yescrypt scratch buffers use it, and this build ships no
   applet that reaches yescrypt.

No behavioural change is intended or made: the patch only removes duplicate definitions
and supplies one libc function bionic lacks.

## What is in the config, and what is not

The config starts from busybox's own `defconfig` — i.e. **all the feature flags upstream
turns on by default**, which is what makes GNU-ish option syntax work: `readlink -f`,
`grep -E`, `sed -i`, `sort -n`, `ps -Ao`, `stat -c`, `find -name … -type f`, `date +%s`,
`killall -l`, `head -n`, `tail -n`, `chattr ±i`.

That matters more than applet count. An earlier attempt built a 104-applet config from
`allnoconfig` plus the applets the scripts name; it booted, the daemon ran, and then the
watchdog died with `readlink: invalid option -- f` — the applet was there, the feature was
not. `libuperf.sh` starts the watchdog through `setsid sh uperf_watchdog.sh`, that script
resolves `readlink -f` to busybox because busybox is first in `PATH`, and busybox without
`CONFIG_FEATURE_READLINK_FOLLOW` rejects `-f`. Lesson: select **applets by inventory and
features by `defconfig`**, never prune features blindly.

Applets disabled on top of `defconfig`, each because bionic cannot build it (missing
headers, or a libc symbol bionic does not export at API 26) or because it cannot work on
Android at all:

| disabled | why |
|---|---|
| `tc` | bionic headers lack the `TCA_CBQ_*` structures |
| `seedrng` | needs `getrandom()` in headers (API 28+) |
| `swapon`, `swapoff` | declarations absent from bionic's `<sys/swap.h>` at API 26 |
| `mount`, `umount`, `losetup`, `switch_root`, `mdev`, `fallocate`, `blkdiscard` | `<mntent.h>`/`<linux/loop.h>` gaps; Android's toybox owns these anyway |
| `chroot`, `setpriv`, `nsenter`, `unshare`, `setarch` | bionic lacks the caps/namespace glue |
| `login`, `su`, `sulogin`, `passwd`, `chpasswd`, `cryptpw`, `mkpasswd`, `vlock`, `adduser`, `addgroup`, `deluser`, `delgroup` | need `<crypt.h>`; no interactive login on a Magisk module |
| `logname`, `who`, `users`, `w`, `last`, `mesg`, `write`, `wall` | need `getlogin()`/utmp |
| `adjtimex`, `hwclock`, `rtcwake`, `ipcs`, `ipcrm`, `readahead` | bionic header gaps; useless for this module |
| `openvt`, `chvt`, `deallocvt`, `fgconsole`, `kbd_mode`, `dumpkmap`, `loadkmap`, `setconsole`, `conspy`, `fbset`, `fbsplash` | `<sys/kd.h>`/console ioctls missing |
| `init`, `linuxrc`, `halt`, `poweroff`, `reboot`, `devfsd`, `crond`, `crontab`, `dnsd`, `ftpd`, `telnetd`, `httpd`, `inetd`, `beep`, `clear`, `reset` | Android's init/toybox own these; several need headers bionic lacks |
| `fsck`, `fsck_minix`, `mkfs_minix`, `mkfs_ext2`, `mkfs_vfat` | ancient filesystem code, no Android use |
| `hush` (`CONFIG_SHELL_HUSH`), `msh` | `ash` is the shell; hush needs `sigisemptyset()`, which bionic lacks |
| `nslookup -big` (`CONFIG_FEATURE_NSLOOKUP_BIG`) | pulls `-lresolv`, which bionic does not ship (resolver lives in libc) |
| `ether-wake`, `arp`, `arping`, `ifconfig`, `ifenslave`, `ifstat`, `fdisk`, `flash*`, `ubi*`, `nandwrite`, `nanddump` | raw ioctl/mtd code that bionic headers do not describe |

Everything else upstream builds stays in: 314 applets, including `vi`, `less`, `top`,
`wget`, `tar`, `unzip`, `xxd`, `flock`, `timeout`, `pgrep`/`pkill`, `awk`, `sed`, `find`,
`xargs`, `diff`, `md5sum`/`sha1sum`/`sha256sum`/`sha512sum`.

To add or remove an applet: edit `busybox-aarch64-uperf.config`, run
`./build-busybox.sh --install`, then update the sha256 in this file and in `build.sh`'s
check. `yes "" | make oldconfig` (which the script runs) resolves dependencies.

## Verifying a build without a device

```
file busybox                                   # ELF 64-bit, ARM aarch64, statically linked
strings -a busybox | grep -m1 '^BusyBox v'     # BusyBox v1.38.0 (2026-01-01 08:00:00 CST)
sha256sum busybox                              # e2b7a98a…
```

On a device, with the binary at `/data/local/tmp/bbnew/busybox` (the name matters — busybox
dispatches on `argv[0]`):

```
$ /data/local/tmp/bbnew/busybox --list | wc -l
314
$ /data/local/tmp/bbnew/busybox readlink -f /data/adb/modules/uperf/service.sh
/data/adb/modules/uperf/service.sh
$ echo abc | /data/local/tmp/bbnew/busybox sed s/b/X/
aXc
$ /data/local/tmp/bbnew/busybox ps -Ao pid,args | head -2      # what libcgroup.sh calls
```

The end-to-end check is the watchdog, because it is the script that resolves the most
applets through `PATH`:

```
M=/data/adb/modules/uperf
PATH=$M/bin/busybox:$PATH setsid sh $M/script/uperf_watchdog.sh &
sleep 20; ps -Ao pid,args | grep uperf_watchdog
head -4 /sdcard/Android/yc/uperf/uperf_watchdog.state    # version=1 / state=running / detail=…
```
