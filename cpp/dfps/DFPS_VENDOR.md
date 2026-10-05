# dfps (vendored) — provenance record

This directory is a **verbatim copy** of the reusable parts of
[`yc9559/dfps`](https://github.com/yc9559/dfps), the same author's open-source project
that Uperf v3's platform layer was split from (dfps `README.md`: *"Splited from Uperf v2"*).

| | |
|---|---|
| Upstream | https://github.com/yc9559/dfps |
| Commit | `f84866c1ff1518da72037056844cb0917a941904` |
| Commit date | 2023-01-15 (`Bump version 23.01.15`) |
| License | Apache-2.0 (`LICENSE` + `NOTICE` kept in place, original file headers untouched) |
| Extracted with | `git -C <dfps> archive HEAD \| tar -x -C cpp/dfps` |
| Files | 210 |
| Local modifications | **one**, `source/modules/input_listener.{h,cpp}` — see below |

## Verification

```bash
git clone https://github.com/yc9559/dfps /tmp/dfps
git -C /tmp/dfps checkout f84866c1ff1518da72037056844cb0917a941904
diff -rq --exclude=.git /tmp/dfps cpp/dfps
# actual output (reproduced 2026-10-05):
#   只在 cpp/dfps 中存在：DFPS_VENDOR.md          <- this file, ours, not part of dfps
#   文件 .../input_listener.cpp 和 .../input_listener.cpp 不同
#   文件 .../input_listener.h   和 .../input_listener.h   不同
# i.e. exactly the two files described below; every other vendored byte is upstream's.
```

## The one local modification

`source/modules/input_listener.{h,cpp}` — a **3-line setter**, nothing else.

dfps hardcodes its input thresholds in the constructor:

```cpp
swipeThd_ = 0.01;
gestureThdX_ = 0.03;
gestureThdY_ = 0.03;
```

uperf reads them from the config (`modules.input`, `config/README.md` lines 96-98
describe all three as live parameters). The upstream uperf binary does contain
`swipeThd`, `gestureThdX` and `gestureThdY` as literals, so it reads them too — wiring
them is **fidelity to upstream, not divergence**. The gap is real, not cosmetic:
**62 of the 63 shipped configs ask for `swipeThd` = 0.03, i.e. 3x the hardcoded
value**, and only `sdm888.json` (the config used for development on alioth) happens to
use 0.01, which is why it went unnoticed locally. `uperf-config`'
`s the_shipped_swipe_threshold_distribution_is_what_justifies_the_wiring` test pins that
distribution, so the claim cannot rot.

The alternative was rewriting `InputListener` in `cpp/uperf/`, which means
reimplementing ~300 lines of evdev hotplug and touch classification that are known to
work. A three-line setter is the smaller risk.

Added:

```cpp
// input_listener.h, public:
void SetThresholds(float swipeThd, float gestureThdX, float gestureThdY);

// input_listener.cpp:
void InputListener::SetThresholds(float swipeThd, float gestureThdX, float gestureThdY) {
    swipeThd_ = swipeThd;
    gestureThdX_ = gestureThdX;
    gestureThdY_ = gestureThdY;
}
```

`gestureDelayTime` and `holdEnterTime` are deliberately **not** wired: README lines
99-100 document both as 暂不使用 (unused), so the vendored constructor's own values are
already correct for them.

Plumbing: `app_main.cpp` registers the constructed instance with the bridge
(`uperf_register_input_listener`), and Rust calls
`uperf_bridge_set_input_thresholds(...)` once it has parsed the config, logging
`Input thresholds: swipeThd=... gestureThdX=... gestureThdY=...` so the applied values
are visible in the device log.

## What is used, and what is not

`cpp/uperf/CMakeLists.txt` selects files explicitly (no `GLOB`), so the following
vendored files are present but **not** compiled into `uperf`:

| Path | Why it is excluded |
|---|---|
| `source/main.cpp` | dfps' process supervisor; reimplemented for uperf in `cpp/uperf/app_main.cpp` (kept as the reference we derived from) |
| `source/dfps.{h,cpp}` | dfps' business layer (module assembly). uperf's equivalent is the Rust `app` module from M1 on |
| `source/modules/dynamic_fps.{h,cpp}` | dfps' only real policy module (variable refresh rate). Uperf's policy modules are Rust |
| `magisk/**` | dfps' own Magisk packaging; uperf uses the module skeleton already in this repo |
| `build.sh`, root `CMakeLists.txt` | dfps' build entry points; this repo has its own `build.sh` / `CMakeLists.txt` that reuse the same compiler and linker flags |

`source/version.c.in` **is** used (via `configure_file`), which is why `version.h` /
`GetGitCommitHash()` work in `app_main.cpp`.

## Rule

Do not edit anything under `cpp/dfps/`. If a vendored file must change, copy it into
`cpp/uperf/`, note the change here, and record the deviation in `AGENT.md`.
