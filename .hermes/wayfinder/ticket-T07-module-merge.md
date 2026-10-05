# T11 — Merge dfps-rs into uperf magisk module

## Question

The endpoint is "embedded into uperf module" (one module.prop, one service.sh,
one webroot, one USER_PATH). What specifically changes in the uperf module to
ship dfps-rs?

1. bin/uperf — does it become the dfps-rs daemon too (single binary, two
   supervisors)? Or do we add bin/dfps?
2. service.sh — does it run both, or just uperf which then forks dfps?
3. module.prop description — does the versionCode bump?
4. customize.sh — anything to seed for dfps (no config to seed if dfps is
   zero-config, but check).
5. NOTICE — vendored cpp/dfps, spdlog, scnlib all deleted. New NOTICE entries:
   none (dfps-rs is in-repo Rust, covered by the existing uperf-rs entry).

## Required output

A short patch list for build.sh + magisk/. No code, just the diff outline.
