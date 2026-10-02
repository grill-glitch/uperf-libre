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
| Local modifications | **none** |

## Verification

```bash
git clone https://github.com/yc9559/dfps /tmp/dfps
git -C /tmp/dfps checkout f84866c1ff1518da72037056844cb0917a941904
diff -rq --exclude=.git /tmp/dfps cpp/dfps && echo "vendored tree is byte-identical"
```

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
