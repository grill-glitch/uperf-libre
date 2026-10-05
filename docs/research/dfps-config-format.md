# Dfps Dynamic-FPS Config File Format — Verbatim Spec

Source: https://github.com/yc9559/dfps (cloned `/tmp/dfps-repo`, branch `main`).
All line citations reference files in that clone. No rules are paraphrased — code is quoted.

---

## 1. Config file path(s) the daemon reads

The daemon is the ELF binary `dfps`. Its CLI grammar (printed by `--help`):

> `Usage: dfps [-o log_file] config_file` — `source/main.cpp:37`
> (the `-n notify_file` flag is also defined — see §3 below; the help string is stale.)

Positional `argv[optind]` becomes the config path, validated for read access:

- `source/main.cpp:175` — `void ParseOpt(int argc, char **argv)`
- `source/main.cpp:204` — `if (len < 1) { SPDLOG_ERROR("Config file not specified"); …`
- `source/main.cpp:208` — `configFile = argv[optind];`
- `source/main.cpp:209` — `if (access(configFile.c_str(), R_OK) != 0) { SPDLOG_ERROR("Config file not found"); …`

In the shipping Magisk module the daemon is launched by `magisk/initsvc.sh` with these exact args:

```sh
# magisk/initsvc.sh:39
DFPS_DIR="/sdcard/Android/yc/dfps"
…
# magisk/initsvc.sh:44
$BASEDIR/bin/dfps $DFPS_DIR/dfps.txt -o $DFPS_DIR/dfps_log.txt -n $DFPS_DIR/dfps_cur.txt
```

So on-device the canonical paths are:

- **Config file:** `/sdcard/Android/yc/dfps/dfps.txt` (read; required argument)
- **Log file:** `/sdcard/Android/yc/dfps/dfps_log.txt` (truncated on start, `-o`)
- **Notify file:** `/sdcard/Android/yc/dfps/dfps_cur.txt` (created on first write, `-n`)

The config file is watched with `inotify` for `CLOSE_WRITE`; a write that closes the file triggers a daemon self-restart to re-parse it:

- `source/main.cpp:160` — `inotify.Add(configFile, Inotify::CLOSE_WRITE, nullptr);`
- `source/main.cpp:166` — `"Config file updated, restart {} to load new config file"`
- `source/main.cpp:167–168` — `KillOldApp(); Sleep(SToUs(0.5)); StartNewApp();`

---

## 2. Schema: keys, value types, units, comments

The parser is `DynamicFps::ParseLine` (called per non-empty line by `LoadConfig`):

- `source/modules/dynamic_fps.cpp:69` — `void DynamicFps::LoadConfig(const std::string &configPath)`
- `source/modules/dynamic_fps.cpp:75` — `char buf[256];` (line-length cap)
- `source/modules/dynamic_fps.cpp:78` — `fgets(buf, sizeof(buf), fp);`
- `source/modules/dynamic_fps.cpp:79` — `auto line = Trim(buf);` — leading/trailing `" \n\r"` stripped (`dynamic_fps.cpp:35–43`)
- `source/modules/dynamic_fps.cpp:80` — `ParseLine(line);`

Line classification (`dynamic_fps.cpp:106–135`):

```cpp
// dynamic_fps.cpp:107
auto isComment = [](const std::string &line) { return line[0] == '#'; };
// dynamic_fps.cpp:108
auto isTunable = [](const std::string &line) { return line[0] == '/'; };
// dynamic_fps.cpp:115
if (line.empty() || isComment(line)) {
    return;
} else if (isTunable(line)) {
    // dynamic_fps.cpp:118: /touchSlackMs 4000
    if (sscanf(line.c_str(), "/%s %s", name, value) == 2) {
        // dynamic_fps.cpp:121
        SetTunable(name, value);
    } else {
        // dynamic_fps.cpp:123
        SPDLOG_WARN("Skipped broken line '{}'", line);
    }
} else {
    // dynamic_fps.cpp:126-127
    // com.example.app 60 120
    // com.example.app 2 0
    FpsRule rule;
    if (sscanf(line.c_str(), "%s %d %d", name, &rule.idle, &rule.active) == 3) {
        // dynamic_fps.cpp:130
        AddRule(name, rule);
    } else {
        // dynamic_fps.cpp:132
        SPDLOG_WARN("Skipped broken line '{}'", line);
    }
}
```

### 2.1 Three line kinds

| Line prefix  | Format                                        | Handler        | Citations                                |
|--------------|-----------------------------------------------|----------------|------------------------------------------|
| `#`          | free-form comment                             | skipped        | `dynamic_fps.cpp:107`, `:115`            |
| (blank)      | —                                             | skipped        | `dynamic_fps.cpp:115`                    |
| `/<key>`     | `/<tunableName> <decimalIntValue>`             | `SetTunable`   | `dynamic_fps.cpp:108`, `:118–124`        |
| `<pkgName>`  | `<token> <idleInt> <activeInt>`               | `AddRule`      | `dynamic_fps.cpp:126–133`                |

Per-line parsing uses `sscanf("%s %d %d", …)`, so tokens are whitespace-delimited, `pkgName` cannot contain spaces, and both rate values must be signed decimal ints. Tokens past the third are ignored. Lines that fail the `sscanf` test are warned and dropped — they do **not** abort config load:

- `dynamic_fps.cpp:123` — `SPDLOG_WARN("Skipped broken line '{}'", line);`
- `dynamic_fps.cpp:132` — `SPDLOG_WARN("Skipped broken line '{}'", line);`

### 2.2 Top-level tunable keys (lines beginning with `/`)

Implementation (`dynamic_fps.cpp:152–162`):

```cpp
void DynamicFps::SetTunable(const std::string &tunable, const std::string &value) {
    if (tunable == "useSfBackdoor") {
        useSfBackdoor_ = (std::stoi(value) > 0) ? true : false;
    } else if (tunable == "touchSlackMs") {
        touchSlackMs_ = std::max(MIN_TOUCH_SLACK_MS, std::stoi(value));
    } else if (tunable == "enableMinBrightness") {
        enableMinBrightness_ = std::min(MAX_ENABLE_MIN_BRIGHTNESS, std::stoi(value));
    } else {
        SPDLOG_WARN("Unknown tunable '{}' in the config file", tunable);
    }
}
```

| Key                  | Type    | Default                            | Range / clamp                       | Meaning                                                                                          | Citations                                  |
|----------------------|---------|------------------------------------|-------------------------------------|--------------------------------------------------------------------------------------------------|--------------------------------------------|
| `/touchSlackMs`      | int ms  | `DEFAULT_TOUCH_SLACK_MS = 4000`    | `>= MIN_TOUCH_SLACK_MS = 100`       | Hold time after touch/button release before idle refresh rate re-engages.                        | `:25`, `:29`, `:156`                       |
| `/gestureSlackMs`    | int ms  | `DEFAULT_GESTURE_SLACK_MS = 4000`  | not parsed from config (hard-coded) | Hold time after a global gesture ends before leaving universal override. (Defined but unused by `SetTunable` — the only settable time tunable is `touchSlackMs`.) | `:26`, `:234`, `:251`, `:280`              |
| `/enableMinBrightness`| int   | `DEFAULT_ENABLE_MIN_BRIGHTNESS = 8`| `<= MAX_ENABLE_MIN_BRIGHTNESS = 255` | Minimum system brightness (0–255) under which dynamic switching pauses (OLED flicker guard).     | `:27`, `:30`, `:158`, `:297`               |
| `/useSfBackdoor`     | int 0/1 | `DEFAULT_USE_SF_BACKDOOR = false`  | `0` → `PEAK_REFRESH_RATE`, `1` → SF backdoor | Pick the refresh-rate-setting backend.                                              | `:28`, `:154`, `:315–319`                  |

Unknown tunables produce only a warning (`dynamic_fps.cpp:160`), so misspelled keys silently no-op.

Brightness sampling cadence (not configurable): `BRIGHTNESS_SAMPLE_INTERVAL_S = 10` (`dynamic_fps.cpp:31`, used at `:294`).

### 2.3 Per-rule lines (default branch of `ParseLine`)

Format, per `sscanf("%s %d %d", name, &rule.idle, &rule.active)` (`dynamic_fps.cpp:129`):

```
<pkgName> <idleValue> <activeValue>
```

Both values are signed decimal `int`s. They are interpreted as Hz **or** as a SF-backdoor config index, depending on `useSfBackdoor_`:

- Per-app values `>= 20` ⇒ Hz supported by the system (used when `useSfBackdoor_ = 0`).
- Per-app values `< 20` ⇒ SF backdoor screen-config index (used when `useSfBackdoor_ = 1`).
- `-1` in either slot ⇒ "use the system default rule" (this combination is the legal sentinel — see §4).
- `magisk/config/dfps_help_en.md:42` — *"The value in the perapp configuration is <20, the value is the screen configuration index supported by the system, and the refresh rate corresponding to 0/1/2/... needs to be tried by yourself."*
- `magisk/config/dfps_help_en.md:38` — *"The value in the perapp configuration >=20, the value is the refresh rate supported by the system, please do not set the frame rate not supported by the system."*

`AddRule` (`dynamic_fps.cpp:137–150`) inserts a `FpsRule{ int idle; int active; }` keyed by the exact package-name token. Lookup at runtime is exact-match against the foreground package via `topapp.pkgName` (`dynamic_fps.cpp:192–193`).

### 2.4 Required rules / load-time validation

`LoadConfig` throws unless both special rules are present, and additionally validates that every rule's values are self-consistent with the chosen backend (`dynamic_fps.cpp:83–95`, `:164–184`):

```cpp
// dynamic_fps.cpp:83-90
if (hasOffscreen_ == false) {
    fclose(fp);
    throw FmtException("Offscreen rule not specified in the config file");
}
if (hasUniversial_ == false) {
    fclose(fp);
    throw FmtException("Default rule not specified in the config file");
}
auto invalidRuleName = FindInvalidRule();
if (invalidRuleName.empty() == false) {
    fclose(fp);
    throw FmtException("Rule of '{}' is invalid", invalidRuleName);
}
```

`FindInvalidRule` (`dynamic_fps.cpp:164–184`):

```cpp
auto isDefaultRule = [](const FpsRule &rule) { return rule.idle == -1 && rule.active == -1; };
auto isSfBackdoorRule = [](const FpsRule &rule) { return rule.idle < 20 && rule.active < 20; };
auto isInvalid = [=](const FpsRule &rule) {
    return isDefaultRule(rule) == false && useSfBackdoor_ != isSfBackdoorRule(rule);
};
```

So a rule is *invalid* — and the daemon refuses to start — if:

- it isn't the literal `-1 -1` "use system default" sentinel for both `idle` and `active`, **and**
- its `(idle < 20 && active < 20)` shape (i.e. SF-backdoor indices) disagrees with the value of `/useSfBackdoor`.

Equivalently: when `useSfBackdoor_ = false` all non-sentinel rules must use values `>= 20`; when `useSfBackdoor_ = true` they must use values `< 20`.

### 2.5 Sample shipped config (`magisk/config/dfps.txt:1–13`)

```
# 动态刷新率配置文件
# Dynamic screen refresh rate controller config
# 修改本配置后，dfps会自动重新加载它
# Dfps will automatically reload this config file after modification

/touchSlackMs 4000
/enableMinBrightness 8
/useSfBackdoor 0

com.miHoYo.Yuanshen 60 60
com.hypergryph.arknights 60 60
- -1 -1
* 60 120
```

---

## 3. Notify file path and contents

The path is whatever was passed via `-n <path>`; in the shipping module it is `/sdcard/Android/yc/dfps/dfps_cur.txt` (`magisk/initsvc.sh:44`). The daemon stores it as `notifyPath_` (`dynamic_fps.cpp:52`) and writes it on every refresh-rate switch (`dynamic_fps.cpp:322–328`):

```cpp
void DynamicFps::NotifyRefreshRate(const std::string_view &hz) {
    int fd = open(notifyPath_.c_str(), O_WRONLY | O_NONBLOCK | O_CLOEXEC | O_CREAT | O_TRUNC);
    if (fd > 0) {
        WriteSysfsFile(fd, hz);
        close(fd);
    }
}
```

`WriteSysfsFile(int fd, std::string_view)` is `return write(fd, s.data(), s.length());` (`source/utils/misc.cpp:193–198`).

Caller chain on each switch (`dynamic_fps.cpp:284–320`):

- `SwitchRefreshRate(bool force)` calls `SwitchRefreshRate(int hz)` with `hz = rule.active` (when a touch/button is pressed) or `hz = lowBrightness_ ? rule.active : rule.idle`.
- `SwitchRefreshRate(int hz)` builds `std::string hzStr = std::to_string(hz);` (`:312`), updates `curHz_` (`:313`), and calls `NotifyRefreshRate(hzStr);` (`:314`) **before** applying the rate to the system.

Therefore:

- **Contents:** the ASCII decimal representation of the integer Hz (or SF-backdoor index) just selected, **with no trailing newline** (it's a raw `write()` of the digits).
- **Open mode:** `O_WRONLY | O_NONBLOCK | O_CLOEXEC | O_CREAT | O_TRUNC` — file is created on first write, truncated on every subsequent write, never deleted by the daemon.
- **Update cadence:** exactly once per `SwitchRefreshRate` call, suppressed only when `force == false && hz == curHz_` (`:308`). Because `curHz_` starts at `INT32_MAX` (`dynamic_fps.cpp:58`), the very first switch is always written.
- The value mirrors whatever the daemon tells the kernel: a SF-backdoor index if `/useSfBackdoor 1`, a Hz value (≥20) otherwise.

---

## 4. Special package-name tokens

Defined as named constants (`dynamic_fps.cpp:32–33`):

```cpp
constexpr char UNIVERSIAL_PKG_NAME[] = "*";   // sic — misspelled "UNIVERSIAL"
constexpr char OFFSCREEN_PKG_NAME[] = "-";
```

(Note the source spelling `UNIVERSIAL` is preserved verbatim throughout the file: see `hasUniversial_` at `:50`, `:87`, `universial_` at `:142`, `:174`, `:196`. The English help doc spells them `*` = default, `-` = offscreen — `magisk/config/dfps_help_en.md:57–58`.)

### 4.1 `*` — default / universal rule

Recognised and stored in `universial_` (`dynamic_fps.cpp:138`, `:140–142`):

```cpp
auto isUniversial = [](const std::string &pkgName) { return pkgName == UNIVERSIAL_PKG_NAME; };
…
if (isUniversial(pkgName)) {
    hasUniversial_ = true;
    universial_ = rule;
}
```

Used at runtime as the fallback when the foreground package isn't in `rules_` (`dynamic_fps.cpp:186–200`):

```cpp
DynamicFps::FpsRule DynamicFps::GetCurrentRule(void) const {
    FpsRule rule;
    const auto &pkgName = overridedApp_.empty() ? curApp_ : overridedApp_;
    if (pkgName == OFFSCREEN_PKG_NAME) {
        rule = offscreen_;
    } else {
        auto it = rules_.find(pkgName);
        if (it != rules_.end()) {
            rule = it->second;
        } else {
            rule = universial_;
        }
    }
    return rule;
}
```

Also used during global gestures: `overridedApp_ = UNIVERSIAL_PKG_NAME;` forces the universal rule for the duration of a gesture (`dynamic_fps.cpp:241–247`).

### 4.2 `-` — offscreen rule

Recognised and stored in `offscreen_` (`dynamic_fps.cpp:139`, `:143–145`):

```cpp
auto isOffscreen = [](const std::string &pkgName) { return pkgName == OFFSCREEN_PKG_NAME; };
…
} else if (isOffscreen(pkgName)) {
    hasOffscreen_ = true;
    offscreen_ = rule;
}
```

Used when the device goes offscreen (`dynamic_fps.cpp:263–282`): `overridedApp_ = OFFSCREEN_PKG_NAME;` while `isOffscreen_ == true`, returning to normal matching on wake. Matched first in `GetCurrentRule` (`dynamic_fps.cpp:189–190`).

Both special rules are **required** — see §2.4.

### 4.3 Rule-priority and lookup order

Per `GetCurrentRule` (`:186–200`) and the help doc (`magisk/config/dfps_help_en.md:56–57`):
> *"The priority of the per-app rules is ordered from high to low."*

Effective resolution order, highest priority first:

1. `overridedApp_ == OFFSCREEN_PKG_NAME` (offscreen state active) → `offscreen_`
2. `overridedApp_ == UNIVERSIAL_PKG_NAME` (global gesture active) → `universial_`
3. Exact match in `rules_` map for `curApp_`
4. `universial_` (default)

`rules_` is filled in **file order** (`std::unordered_map` at `dynamic_fps.cpp:148` — `rules_.emplace(pkgName, rule);`). Because lookup is by exact package-name equality and there is no glob/wildcard per-app syntax, the only "fallback" rule in practice is `*`. The shipped config has the universal rule `* 60 120` last (`magisk/config/dfps.txt:13`), which doubles as the safe default.

---

## 5. Quick reference (all literal values)

- Default tunable values: `touchSlackMs=4000`, `gestureSlackMs=4000`, `enableMinBrightness=8`, `useSfBackdoor=0` (`dynamic_fps.cpp:25–28`)
- Tunable clamps: `MIN_TOUCH_SLACK_MS=100`, `MAX_ENABLE_MIN_BRIGHTNESS=255` (`dynamic_fps.cpp:29–30`)
- Brightness resample interval: `BRIGHTNESS_SAMPLE_INTERVAL_S=10` (`dynamic_fps.cpp:31`)
- Sentinel rule (use system default): both `idle` and `active` equal `-1` (`dynamic_fps.cpp:165`)
- Backend boundary: `< 20` ↔ SF-backdoor index, `>= 20` ↔ Hz (`dynamic_fps.cpp:166`, `dfps_help_en.md:38,42`)
- Config line buffer: `char buf[256]` (`dynamic_fps.cpp:75`) — longest line is 255 bytes including the newline
- Config-package-token cap: `char name[256]` (`dynamic_fps.cpp:110`) — package names are likewise ≤ 255 bytes
- Initial `curHz_` sentinel: `INT32_MAX` (`dynamic_fps.cpp:58`)
