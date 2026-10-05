# M8 — KernelSU WebUI

Status of every claim below: **[V]** verified by tool output, **[I]** inferred, **[U]**
unknown.

## 1. What it is

KernelSU serves `<module>/webroot` as a WebView page inside the manager, and injects a
bridge (`window.ksu`) that lets that page run commands as root. This module ships such a
page: three tabs — work state, mode switching, more — over the same control entry the
shell tests use.

The stack follows the reference implementation the request named
(`KernelSU-Next/KPatch-Next-Module/tree/main/webui`, MIT): Vite with `base: './'`,
`@material/web` for the Material 3 components, `kernelsu-alt` for the bridge, XML locale
files, a hash-free `.page`/`.bottom-bar` router, and the manager's own theme variables.
The WebUI source is `webui/`; the build output goes to `magisk/webroot/` and is not
committed (`.gitignore`), exactly like the binary — `build.sh pack` builds it, and
`build.sh check` refuses a stale or unbuilt one.

## 2. The control contract [V]

`magisk/script/webui.sh` is the single entry point the page uses, and it is also how the
whole thing was tested from adb — no manager needed. Every subcommand prints `key=value`
lines, so the frontend never parses prose.

| command | what it does |
|---|---|
| `webui.sh info` | SoC/device/Android/kernel/SELinux, `module.prop` fields |
| `webui.sh status` | daemon pids, `$USER_PATH` and config path + sha256, current preset, governor state, log path/size, uptime |
| `webui.sh all` | `info` then `status`, one round trip |
| `webui.sh set-preset <name>` | writes `cur_powermode.txt`, then **reads it back** and reports what the file actually contains |
| `webui.sh log [n]` | tails the log (default 200, capped 2000) |
| `webui.sh restart` | `uperf_stop` + `uperf_start`, reporting the pid count before and after |

Measured on alioth, with a live daemon:

```text
$ sh webui.sh status
daemon.count=2
daemon.pids=9837 9839
config.present=1
config.sha256=ef97c9d324a5338ac9fd11f4a03b941c6c0b68fea6b57180cdc4b8ed4dbf8d7a
preset.current=balance
governor.takeover=0
governor.policies=policy0=schedutil policy4=schedutil policy5=schedutil
log.lines=18435
```

and the write path, end to end:

```text
$ sh webui.sh set-preset powersave
preset.want=powersave
preset.set=powersave
preset.ok=1
$ sh webui.sh status | grep preset.current
preset.current=powersave
```

The RESTART path is what exposed four real defects in the module's own scripts; they are
written up in `docs/m7-evidence.md` §7. The two that mattered most: a lost `$USER_PATH`
used to disable the module permanently (now self-healing), and the daemon used to die of
`SIGPIPE` ~90s after any shell-scoped restart (now it ignores `SIGPIPE` and the launcher
detaches its stdio) — which is exactly what a WebUI restart would have triggered.

## 3. What each tab reads and writes

**首页** — daemon state (`daemon.count`, pids, uptime), governor takeover, the loaded
config (file name, `meta.name`, first 16 hex of the sha256), the current preset, the
device rows, and the module's own `module.prop` fields. Read-only.

**模式切换** — the preset list is built from the **loaded config's** `presets` keys plus
`auto` [V]. That is not a stylistic choice: `sdm865.json`, the config this device runs,
defines five presets (`balance`, `powersave`, `performance`, `fast`, `crazy`), so a
hardcoded four-name list would have offered preset names the config cannot resolve. A
tap writes the file and then re-reads it; the UI re-renders from that read, not from the
tap. `auto` is shown because it is a legal value of the file, and its hint says only what
is actually known — that it hands switching to the per-app rules (`switcher.rs` marks the
fuller semantics **[I]**).

**更多** — the log tail (100/500/2000 lines, refreshed on tap), a restart action behind a
confirmation dialog, the language row, and the project links (source, upstream uperf,
the vendored dfps, Apache-2.0, attribution).

## 4. Verification

### 4.1 Rendering and interaction, driven by real device output [V]

The page is a thin layer over shell commands, so the checks that matter are: does it
render the real data, does a tap issue the real command, and does it show the result of
the write rather than the intent. It was driven in a headless browser with a stub
`window.ksu` that replays **output captured from the device** (`webui.sh all`,
`uperf.json`, `webui.sh log 100`), made stateful so a preset write changes what the next
status read returns.

```text
errors:               []                       <- no JS errors in any flow
status:               运行中 | pids 9837 9839 · 已运行 26m | 未接管调频器
config:               uperf.json | sdm865/sdm865+/sdm870[22.09.04] | 当前预设: balance · SHA-256: ef97c9d324a5338a
device:               型号 M2012K11AC / SoC kona·SM8250 / Android 16 · API 36 / 4.19.325-cip131… / Enforcing
mode rows:            balance(true), powersave, performance, fast, crazy, auto
preset write:         toast "已切换到 省电 (powersave)"; next status read -> Extreme (crazy) reflected
restart:              confirm dialog -> "uperf 已重启"
language:             首页/模式切换/更多 -> Home/Mode/More
commands issued:      sh …/webui.sh all | cat '…/uperf.json' | sh …/webui.sh set-preset 'powersave' |
                      sh …/webui.sh restart | sh …/webui.sh log '100'
```

The command strings the page issues are byte-identical to the adb-verified ones. The
first render test found two real bugs and one harness bug, all fixed: an icon name
(`person`) that is not in the generated set (the generator target fails loudly rather
than rendering nothing), a uniformly-quoted command that made the shipped string differ
from the tested one, and the stub's own argument pattern.

### 4.2 Negative case: no manager [V]

Opened outside the manager there is no bridge, and the page says so instead of showing
empty cards: `Open this page from the KernelSU manager: the shell bridge is unavailable.`
**[V]**

### 4.3 What is *not* verified here [U]

The page rendering **inside the manager's WebView** has not been observed by me — that
needs the manager UI on the device. The module files are installed and the manager
(`com.rifsxd.ksunext`) is present [V]; what remains is opening the module's WebUI and
confirming it paints. Everything it depends on (the bridge contract, the commands, the
data shapes, the locale files) is verified above, and the theme degrades to literal
Material 3 colours when the manager's own CSS is unavailable [V — every variable has a
fallback].

## 5. Build

```sh
export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make pack check
```

`pack` builds the WebUI (icon generation from `@material-symbols/svg-400`, then vite) and
stages `magisk/webroot` into the zip; `check` fails if the webroot is missing, if the
bundle does not reference the control script, or if `kernelsu-alt` is still an unresolved
import. Measured: `webui : index.html + index-CkpJpgai.js (480543 bytes), imports
resolved`. The bundle is ~480 kB because `@material/web` is bundled whole (87 kB gzipped);
it is served from the device, so this trades size for the reference's component set.
