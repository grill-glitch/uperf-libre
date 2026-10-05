# T10 — Notify file: same path or new?

## Question

dynamic_fps writes the active refresh rate index to a file (notifyPath_), opened
O_CREAT|O_TRUNC, written, closed. uperf-rs already watches a single-byte file
(sfanalysis.hint) under the same USER_PATH. Decide:

1. Reuse uperf-rs's USER_PATH for dfps notify file? Pro: one path for the
   user to inspect. Con: dfps's current users (vendored) may have an existing
   path on devices already running upstream dfps — must not break.
2. New path /data/dynamic_refresh_rate (vendored default)? Pro: no migration
   risk for upstream dfps users. Con: lives outside USER_PATH, harder to
   inspect via WebUI.

## Evidence required before deciding

The vendored dfps binary's compile-time default for notifyPath_ (grep source
for `dynamic_refresh_rate` or `notifyPath_` defaults). If vendored ships no
default and the path is config-driven, that's a different conversation.

## Required output

One paragraph: the chosen notify file path + one-line rationale tied to
backward compatibility for upstream dfps users.
