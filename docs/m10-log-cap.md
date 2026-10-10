# M10 — the daemon's log is bounded

## The problem, measured

On alioth before this change:

| file | size |
|---|---|
| `/sdcard/Android/yc/uperf/uperf_log.txt` | 34,365,841 bytes (≈34 MB, about a day of uptime) |
| `/sdcard/Android/yc/uperf/uperf_log.txt.bak` | 138,046,729 bytes (≈138 MB) |

≈172 MB of the user's `/sdcard` for a module whose selling point is a small
userspace daemon. Two causes, not one: the daemon appended to a single file
forever, and `uperf_start` moves that file aside on every start
(`mv uperf_log.txt uperf_log.txt.bak`) while nothing ever trimmed the copy — so the
backup grew across runs and the live file grew within one.

## What changed

Two layers, because each covers what the other cannot.

1. **In the writer — `cpp/uperf/app_main.cpp`.** spdlog's `rotating_file_sink_mt`
   replaces `basic_file_sink_mt`: 4 MiB per file, 2 rotated files, so the live set is
   bounded at ≈12 MiB — `uperf_log.txt` plus `uperf_log.1.txt` / `uperf_log.2.txt`
   (this spdlog inserts the index *before* the extension; see
   `rotating_file_sink.h`). Only the writer can bound this
   without re-reading a hundred megabytes, and a script cannot rotate a file the
   daemon holds open. `UPERF_LOG_MAX_BYTES` overrides the budget — the device harness
   forces 64 KiB so a rotation is observable in a minute rather than a day.
2. **In the scripts — `magisk/script/libuperf.sh`.** `uperf_trim_log_backup()` runs
   immediately after the `mv` and *drops* a backup over `UPERF_LOG_BACKUP_MAX_BYTES`
   (default 16 MiB) rather than reading it: trimming 138 MB on `/sdcard` at boot would
   cost more than the space it frees. It reads `$USER_PATH` at call time, so the
   harness can redirect it (the same late-binding rule the watchdog needed).

Neither layer changes the log's contract: same path, same `H:M:S L message` lines,
`webui.sh status` still counts `uperf_log.txt` lines and reports `.bak` existence.
What changes is that "the log" may be split across `.1`/`.2` — documented here so
nobody wonders where the older lines went.

## Evidence

**[V] Host** — `scripts/test_watchdog_host.sh` case 10 sources the real `libuperf.sh`
and checks both directions: a backup over the cap is dropped (and reported), one under
the cap is kept byte-exact.

**[V] Device, rotation** — `scripts/m9-device-verify.sh` part 4 starts the daemon with
`UPERF_LOG_MAX_BYTES=8192` on the real binary for 60 s: `daemon_log.1.txt` and
`daemon_log.2.txt` appear capped at 8 KiB while the base drops back to a few hundred
bytes. (A cap *above* the daemon's real output made this a coin flip: measured 39 KB,
56 KB, 57 KB in three 60 s runs at 64 KiB, and raising `modules.log.level` to `trace`
did not change the volume — it is activity-bound, not level-bound.)

**[V] Device, after install + a restart** — `scripts/m9-device-boot-verify.sh` part 4,
on a freshly installed module: `uperf_log.txt` 849,395 bytes, `.bak` 1,204,302 bytes,
both inside their bounds (against 172 MB before).

**[U]** A full day at info level is inferred from the rotation being size-driven, not
verified by waiting a day.

## Knobs

| variable | default | where |
|---|---|---|
| `UPERF_LOG_MAX_BYTES` | 4194304 | daemon (spdlog), per file; 2 rotated files; values under 4 KiB are ignored |
| `UPERF_LOG_BACKUP_MAX_BYTES` | 16777216 | `libuperf.sh`, the copy the start path makes |
