# T09 — SwitchRefreshRate call-frequency control

## Question

dynamic_fps.cpp's SwitchRefreshRate(force=false) is called from many event
handlers (input.touch every finger-frame, topapp.switch, etc.). The sf backdoor
is `service call SurfaceFlinger 1035 i32 N` — a binder transaction that is
nontrivial. Decide:

1. Should dfps-rs call the backdoor every event, or dedupe by (target_hz,
   in-flight token)? dfps does dedupe by comparing new vs current rule; only
   switches when changed.
2. force=true means full flexibility-token dance (1036 1 → 1035 -1 → 1035 idx →
   1036 0). Is force=true actually used anywhere upstream, or vestigial?
   (grep dynamic_fps.cpp + main.cpp for `force=true` callers.)

## Evidence

- source/modules/dynamic_fps.cpp:97-99 (useSfBackdoor path)
- source/modules/dynamic_fps.cpp:245-260 (SyncCallSurfaceflingerBackdoor)
- source/modules/dynamic_fps.cpp:302-321 (SwitchRefreshRate overloads)

## Required output

One paragraph: dedupe strategy + whether force=true survives.
