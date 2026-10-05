# T12 — WebUI: add dfps tab to uperf WebUI

## Question

The endpoint is "WebUI three-tab can switch fps rules". Decide:

1. Add a 4th tab "刷新率" to the existing 3-tab WebUI (首页/模式切换/更多)?
2. The dfps tab reads/writes what files?
3. The dfps-rs daemon needs its own control entry analogous to webui.sh.
   Reuse one shell script or two?

## Constraints

The existing WebUI must continue to work for uperf-only installs. The dfps tab
must work only when the dfps-rs daemon is part of the build.

## Evidence

- webui/index.js:63-77 (existing setupRoute)
- webui/index.html:42-56 (existing 3 pages)
- magisk/script/webui.sh (existing control entry pattern to copy)

## Required output

A short list: tab label, panel content, control entry name.
