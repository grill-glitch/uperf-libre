# KernelSU WebUI

Status of every claim below: **[V]** verified by tool output, **[I]** inferred, **[U]**
unknown. Section 4 is the reasoning behind the layout; section 5 is what was actually
checked and what was not.

## 1. What it is

KernelSU serves `<module>/webroot` as a WebView page inside the manager, and injects a
bridge (`window.ksu`) that lets that page run commands as root. This module ships such a
page: four tabs — work state, mode switching, refresh-rate rules, log/actions — over the
same control entry the shell tests use.

The stack follows the reference implementation the request named
(`KernelSU-Next/KPatch-Next-Module/tree/main/webui`, MIT): Vite with `base: './'`,
`@material/web` for the Material 3 components, `kernelsu-alt` for the bridge, XML locale
files, a hash-free `.page`/`.bottom-bar` router, and the manager's own theme variables.
The WebUI source is `webui/`; the build output goes to `magisk/webroot/` and is not
committed (`.gitignore`), exactly like the binary — `build.sh pack` builds it, and
`build.sh check` refuses a stale or unbuilt one.

Source layout: `index.html` (markup) + `styles.css` + `theme.css` (colour roles),
`index.js` (state and wiring), `route.js` (tabs), `language.js` (i18n), `ctl.js` (the
**only** module that talks to the device), `ui.js` (shared DOM helpers and the one
explainer dialog), `presets.js` (preset names + the write path), `pages/*.js`,
`icons.js` (generated).

## 2. The control contract [V]

`magisk/script/webui.sh` is the single entry point the page uses, and it is also how the
whole thing was tested from adb — no manager needed. Every subcommand prints `key=value`
lines, so the frontend never parses prose, and every write is read back from the file
rather than assumed.

| command | what it does |
|---|---|
| `webui.sh info` | SoC/device/Android/kernel/SELinux, `module.prop` fields |
| `webui.sh status` | daemon pids, `$USER_PATH` and config path + sha256, current preset, governor state, log path/size, uptime |
| `webui.sh all` | `info` then `status`, one round trip |
| `webui.sh set-preset <name>` | writes `cur_powermode.txt`, then **reads it back** and reports what the file actually contains |
| `webui.sh log [n]` | tails the log (default 200, capped 2000) |
| `webui.sh export [n]` | copies a slice of the log to `$USER_PATH/export/uperf_log_<stamp>.txt` and reports the path, line count and byte count |
| `webui.sh restart` | `uperf_stop` + `uperf_start`, reporting the pid count before and after |

`export` is the one subcommand added for the UI pass. It exists because the log is shown
in a WebView `<pre>`, which on a phone cannot be selected and copied out of: without it
there is no way to get the text off the device at all. It writes inside the module's own
user directory (FUSE, writable as root, removed with the module) and verifies the file it
produced, so a `tail` that silently failed cannot be reported as success.

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

and the write paths, end to end:

```text
$ sh webui.sh set-preset powersave          # -> preset.ok=1, next status read agrees
$ sh webui.sh export 137                    # -> export.lines=137 export.ok=1
$ sh webui.sh export                        # -> export.lines=2000 (default, capped)
$ sh webui.sh export abc                    # -> non-numeric falls back to 2000
$ sh dfps.sh set-rule com.hermes.previewtest 45 90
                                            # -> rule.ok=1, `info` then lists 4 rules
```

The RESTART path is what exposed four real defects in the module's own scripts; they are
written up in `docs/m7-evidence.md` §7. The two that mattered most: a lost `$USER_PATH`
used to disable the module permanently (now self-healing), and the daemon used to die of
`SIGPIPE` ~90s after any shell-scoped restart (now it ignores `SIGPIPE` and the launcher
detaches its stdio) — which is exactly what a WebUI restart would have triggered.

## 3. What each tab reads and writes

**首页** — a hero card (daemon state, uptime/pid/armed-cluster numbers, and an
*interactive* governor badge), the loaded config plus a one-tap preset chip row, the
supervision rows, and the device/module facts behind a disclosure. Read-only apart from
the preset chips, which make the same write as the 模式切换 tab.

**模式切换** — one radio list. The list is built from the **loaded config's** `presets`
keys plus `auto` [V]. That is not a stylistic choice: `sdm865.json`, the config this
device runs, defines five presets (`balance`, `powersave`, `performance`, `fast`,
`crazy`), so a hardcoded four-name list would have offered preset names the config cannot
resolve. A tap writes the file and then re-reads it; the UI re-renders from that read,
not from the tap. `auto` is a row of the same list (it is one of the choices) and its
explanation is the row's subtitle; the fuller explanation lives behind the ⓘ.

**刷新率** — the current Hz, the rule list, and a dialog that edits one rule or adds one.
Reads `dfps.sh status|info`; writes `dfps.sh set-rule <pkg> <idle> <active>`. The same
call adds and replaces, so the same dialog does both: `pkg` is a field when adding and
fixed text when editing.

**更多** — the log (100/500/2000 lines, wrap toggle, copy, export, full screen), a restart
action behind a confirmation dialog, the language row, and the project links.

## 4. Why the layout is what it is

The pass that produced this version started from four concrete complaints about the
previous one. Each is answered by a mechanism, not by restyling:

| Complaint | What changed |
|---|---|
| Every screen is the same dark rounded card; nothing says what matters, no colour for "fine" vs "wrong" | The status card became a hero that is allowed to differ: an accent-tinted container, a filled icon chip, a tinted `error` container when the daemon is down, and a three-tile number strip. Colour comes only from the manager's `--md-sys-color-*` roles (accent = `primary`, failure = `error`), so the user's dynamic colour still drives it — nothing hardcodes a brand hue. Cards also gained a 1px outline so they separate against the background instead of stacking as one block. |
| Labels and values have the same weight, so a screen of "Kernel: 4.19.325-cip131-…" is unreadable | One shared `.kv` grid: labels are small, muted and left; values are larger, brighter, tabular-figured. Reference material (SHA-256, paths, kernel string, `module.dir`) is marked `.mono` and rendered visibly secondary. The rows that were a single grey sentence (`型号: M2012K11AC`) now scan as a column. |
| The home page is one long undifferentiated scroll; SoC/kernel/ABI dominate the fold | The fold is what changes: state, the numbers, the config and the preset chips. Device + module facts moved into a collapsed disclosure with a summary line, and the module description (the longest string in the module) lives inside it. |
| On 模式切换 the current preset appears three times (a card at the top, a row in the list, a pinned card at the bottom with the same sentence), and "defined by the loaded config" is repeated under every entry | The list *is* the state: the current entry is the checked radio. `auto` is one row of that list, not a second card. The filler subtitle is gone (only `auto` gets one, and it says something). The explanation moved behind the ⓘ, and the still-useful file path stayed as the one hint under the list. |
| The ⟳ in the top bar is on all four tabs and its scope is unclear; the "未接管调频器" pill looks like a static badge; the log box is cramped with no way to get the text out | The top-bar button is now a labelled action whose label follows the tab (`Refresh state` / `Reload` / `Refresh log`), and carries `title` + `aria-label`. The governor pill has a border, a state dot, an ⓘ glyph and a ripple, and opens a dialog that explains what takeover means, why it is off by default on this kernel, and what the policies and the recorded values currently are. The log wraps by default (the daemon's sched lines are ~150 columns), toggles full screen, copies, and exports to a real file. |

Two smaller consistency rules came out of the same pass and are worth keeping:

* **Nothing is said twice.** The hero subtitle no longer repeats the uptime that the
  stat strip shows right below it; the mode page's section label says `预设` instead of
  repeating the tab title.
* **The page never invents a value.** A missing field is omitted or shown as `—`; a
  state string the scripts grew that the page does not know is shown verbatim
  (`home.js:stateLabel`). With no bridge at all, the hero carries the one message and the
  data cards are hidden rather than rendered as a grid of empty values.

## 5. Verification

### 5.1 Rendering and interaction, driven by real device output [V]

The page is a thin layer over shell commands, so the checks that matter are: does it
render the real data, does a tap issue the real command, and does it show the result of
the write rather than the intent. It was driven in a headless browser at the device's own
logical viewport (alioth: 1080x2400 @440 → 393x873dp, 2x) with a stub `window.ksu` that
replays **output captured from the device** (`webui.sh all`, `uperf.json`,
`webui.sh log 100|500`, `dfps.sh status|info`), made stateful so a preset write changes
what the next read returns.

```text
errors:               []                       <- no JS errors in any flow (all four tabs,
                                                 both themes, both locales, every dialog)
home:                 运行中 | pids 6325 6326 · 18m | 未接管调频器
stats:                运行时长 18m / 守护 PID 6325 6326 / 已接管集群 0
config:               uperf.json | sdm865/sdm865+/sdm870[22.09.04] | SHA-256 ef97c9d324a5338a
                      | /sdcard/Android/yc/uperf/uperf.json
chips:                自动[selected] 省电 均衡 性能 快速 极致
supervision:          看门狗 运行中 | 看门狗 PID 6773 | 重启次数 0 | 守护进程自报 运行中
device (collapsed):   disclosure-body            <- hidden until asked for
device (expanded):    6 rows; module: 5 rows (incl. the full description)
mode rows:            auto:true powersave:false balance:false performance:false fast:false crazy:true
                      (after tapping `crazy`: auto:false … crazy:true, and the home chips
                       follow — the write→read→redraw chain, not a local guess)
dfps:                 60 Hz | 3 rules | 所有应用 (*) | 熄屏 (-) | com.android.settings
rule dialog (edit):   title "com.android.settings 的规则", pkg field hidden, 90/120
rule dialog (add):    pkg field shown
log:                  2409 行 · /sdcard/Android/yc/uperf/uperf_log.txt
export:               toast "已导出到 /sdcard/…/export/uperf_log_20261010-233558.txt";
                      the meta line then shows the export path
copy:                 toast "日志已选中，长按可复制" (the clipboard was refused in this
                      browser, so the documented fallback ran: the log text is selected)
log full screen:      log-card.fullscreen, 744px of an 873px viewport, body.log-open set
no-bridge hero:       Open this page from the KernelSU manager: the shell bridge is unavailable.
no-bridge sections:   config/supervision/device all hidden; a later normal load restores them
commands issued:      sh …/webui.sh all | cat '…/uperf.json' | sh …/dfps.sh status |
                      sh …/dfps.sh info | sh …/webui.sh set-preset 'crazy' |
                      sh …/webui.sh log '100'
```

The command strings the page issues are byte-identical to the adb-verified ones. Four
defects were found by these runs and fixed:

* `#supervision-card` had no id, so the new error-state hide loop threw on it.
* The fixed 31% label column wrapped a 19-character unbreakable config identity onto a
  second line.
* **All four bottom-bar icons were invisible.** `.bottom-bar-item .bottom-icon` sets
  `width: 24px; padding: 2px 12px`, and under the page-wide `box-sizing: border-box` the
  horizontal padding consumed the whole 24px, so `svg { width: 100% }` resolved to 0 —
  leaving the active pill with nothing in it. This one is inherited from the reference
  implementation's stylesheet (it is visible in the "before" screenshot too). The fix is
  `box-sizing: content-box` on that element, which also restores the pill to its intended
  48x28 (Material 3's active-indicator shape) instead of a 24px circle.
* **The hero status glyph was invisible** — same class of bug, different mechanism. The
  icon measured a perfectly correct 26x26 with a non-empty path, but `.glyph` sets
  `color: primary` on the icon itself, and the chip behind it is *also* `primary`:
  computing `fill` against the nearest painted background gave `rgb(208,188,255)` on
  `rgb(208,188,255)`. Fixed by letting the chip's foreground win
  (`.icon-chip .glyph { color: inherit }`), which also fixes the selected chip's tick.

The two invisible-icons cases show why the audit has to be run **twice, on two different
axes** — a box measurement catches the first and is blind to the second:

* every `getElementById` target in `webui/*.js` and `webui/pages/*.js` exists in
  `index.html` — currently none missing;
* every `md-icon` **on the visible page** has an `<svg>` with a non-empty `path` and a
  rendered box of at least 6x6 — currently none bad on all four tabs (a hidden page
  measures 0x0 by definition, so each tab has to be made active before auditing it);
* and the glyph's computed `fill` has a contrast ratio of at least 1.6 against the first
  non-transparent background above it — currently clean on all four tabs, both themes,
  with the info dialog open, in the full-screen log, **and** in the no-bridge error state
  (which paints the chip with `error`/`on-error` instead of `primary`/`on-primary`).

That last one also turned up a hole in `theme.css`: its dark block never restated
`on-secondary`, `on-tertiary` or `on-error`, so those kept their light-scheme values —
white `on-error` on the dark scheme's `error` (#F2B8B5) is unreadable. The missing
Material 3 dark baselines are now there (`on-error` is #601410).

The icon set is generated, and `tools/gen-icons.mjs` fails the build in both directions:
a listed icon the package does not have, and a name a call site uses that is not listed.
It now also scans `glyph()`/`setGlyph()` (the wrapper `ui.js` puts in front of the icon
map) and only the first argument of those calls, so a class name like `'dim'` is not
reported as an icon and a name behind the wrapper is not skipped.

### 5.2 Reproducing it locally [V]

`webui/tools/preview-server.py` is the harness, minus the browser: it serves
`magisk/webroot` and injects a stand-in bridge built from payloads it captures itself.

```sh
cd webui && npm run build
python3 tools/preview-server.py --capture --serial <serial> --port 8097
# -> http://127.0.0.1:8097/ (binds 0.0.0.0, so the phone's browser can open it too)
```

It is a viewer, not a test rig: it never writes to the device. That is why the write
paths were verified against the real scripts instead (5.3).

### 5.3 Device-side verification of the write paths [V]

Everything the page can write was exercised against the installed module on alioth:

* `webui.sh set-preset` → `preset.ok=1`, and the next `status` read agrees.
* `webui.sh export 137` / `export` / `export abc` → 137 lines, 2000 (cap), 2000 (fallback);
  the produced file was read back off the device and matches the log's tail.
* `dfps.sh set-rule com.hermes.previewtest 45 90` → `rule.ok=1`, `info` then lists 4 rules
  (up from 3). The rule file was backed up first and restored afterwards; the file's
  sha256 prefix is identical before and after (`3d2a54286654a15c`), so the device's own
  tuning was left untouched.
* `sh -n` on every script, and `scripts/test_watchdog_host.sh` → **109 passed, 0 failed**
  (it exercises `webui.sh status` without a phone).
* `sh build.sh Release check` → green, including the webroot gate
  (`index.html + index-*.js, imports resolved`).

### 5.4 Negative case: no manager [V]

Opened outside the manager there is no bridge, and the page says so instead of showing
empty cards: `Open this page from the KernelSU manager: the shell bridge is
unavailable.` The data cards are hidden and the loading a normal bridge restores them.

### 5.5 What is *not* verified here [U]

* The page rendering **inside the manager's WebView** has not been observed by me — that
  needs the manager UI on the device. The module files are installed and the manager
  (`com.rifsxd.ksunext`) is present [V]; what remains is opening the module's WebUI and
  confirming it paints. Everything it depends on (the bridge contract, the commands, the
  data shapes, the locale files) is verified above, and the theme degrades to literal
  Material 3 colours when the manager's own CSS is unavailable [V — every variable has a
  fallback].
* `navigator.clipboard` inside that WebView is **[U]**. The copy button tries it, then the
  selection-based `execCommand('copy')`, then selects the log and says so [V — the third
  path ran in the harness, where the first two were refused]; which one wins on the device
  is untested.
* The dfps `set-rule` HTTP-equivalent on a *brand-new* package was verified through the
  script (5.3), not through the dialog's own field: the dialog's payload shape was only
  observed in the harness. The command string is identical to the one tested.

## 6. Build

```sh
export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make pack check
```

`pack` builds the WebUI (icon generation from `@material-symbols/svg-400`, then vite) and
stages `magisk/webroot` into the zip; `check` fails if the webroot is missing, if the
bundle does not reference the control script, or if `kernelsu-alt` is still an unresolved
import. Measured: `webui : index.html + index-BoIIeJBy.js (494095 bytes), imports
resolved`. The bundle is ~494 kB because `@material/web` is bundled whole (~91 kB
gzipped); it is served from the device, so this trades size for the reference's component
set.

A UI-only change therefore needs nothing but `cd webui && npm run build`, then the
preview server to look at it.
