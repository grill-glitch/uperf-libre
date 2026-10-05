# M6d — atrace, and the `pinned` / top-app path verified live

Status legend: **[V]** verified by tool output · **[I]** inferred · **[U]** unknown.

## 1. `atrace` — resolved by reusing dfps' own utility [V]

This was carried as **[U]** ("the marker payload is not in the binary") for two
milestones. It is now implemented, and the resolution came from the vendored tree
rather than from guessing: **`cpp/dfps/source/utils/atrace.c` is the same author's
implementation of exactly this mechanism**, and it defines the payloads.

| Evidence | Value |
|---|---|
| uperf binary strings | `/sys/kernel/debug/tracing/trace_marker`, `/sys/kernel/tracing/trace_marker`, `AtraceSwitcher`, `Failed to open tracemark for atrace` |
| `atrace.c` | the same two paths, in the same order, as an `open()` with that exact fallback |
| marker formats | `B|<pid>|<tag>` / `E|<pid>` (scopes) and `C|<pid>|<tag>|<n>` (counters) |
| the switch | `AtraceToggle(bool)` — i.e. `modules.atrace.enable` |
| the payloads | come from the `ATRACE_CALL()`/`ATRACE_SCOPE()` macros **already instrumented** into dfps' code: `inotify.cpp:59` `ATRACE_SCOPE(InotifyHandle)` and `topapp_monitor.cpp:50` `ATRACE_SCOPE(GetTopAppName)` |

That is why the uperf binary has no marker literal: the switcher only toggles a flag,
and the payloads live at the instrumented call sites. The absence of a literal was
the wrong thing to look for.

**A build-level check worth keeping**: before the change, `trace_marker` and `B|%d`
were **absent** from the binary — `--gc-sections -flto` drops `AtraceInit` entirely
when nothing calls it. After wiring the call they are present:

```text
trace_marker -> 2   (both paths)
B|%d         -> 1   (the scope format, used by ATRACE_SCOPE)
```

So "does the binary call AtraceInit" is a one-line grep, and the upstream binary
having both paths is itself the evidence that upstream calls it.

**Device verification** (`modules.atrace.enable = true`, `tracing_on` saved/restored):

```text
manual probe:  printf 'B|99999|uperf_probe' > trace_marker  -> 1 line in the buffer
Rust:          Atrace enabled
from our pids: 34 marker lines
sample:        Inotifier-28098 [003] .... 30121.462380: tracing_mark_write: B|28095|InotifyHandle
```

34 real markers from our worker, in exactly the `B|<pid>|<tag>` shape the vendored
utility writes, tagged `InotifyHandle` from `inotify.cpp:59`. The supervisor daemon
emitted **0** — which matches the vendored `main.cpp`, which never touches atrace, so
the structure is the same as upstream's.

On alioth only the **fallback** path exists: `/sys/kernel/debug/tracing/trace_marker`
is missing and `/sys/kernel/tracing/trace_marker` is writable — the fallback earning
its place.

## 2. `pinned` and the real top-app path [V]

AGENT.md §8.5 and `config/README.md` line 248 define `pinned` as "始终作为`处于顶层
可见的进程`应用规则" — the rule is always evaluated as if its process were the
top-visible one. That was implemented from the README and had only ever been exercised
in a synthetic dry run. Live, on the shipped config, driving the top app with
`am start`:

```text
before:                                   sched scene=idle top=None
after going home:                         sched scene=idle top=Some("com.android.launcher3")
after launching Settings:                 sched scene=idle top=Some("com.android.settings")

Settings' threads, scenes in order:       90x bg   ->   209x idle
pinned processes ever at scene=bg:        0
```

Read that as the two halves of the semantics:

* a **non-pinned** rule (Settings matches the trailing `"regex": "."` Default rule)
  sits in `bg` and switches to the FSM scene (`idle`) exactly when its process
  becomes the top app;
* a **pinned** rule's processes (`surfaceflinger`, `system_server`,
  `com.android.systemui` in this config) never take `bg` — 0 occurrences across the
  run — because they are always evaluated as top-visible.

`top=` also confirms the `topapp.pkgName` event path works end to end
(dfps' TopappMonitor -> orchestrator -> the applier's `is_top_app`).

**Behavioural note worth knowing**: `top_app` starts as `None` and only changes when
a top-app event arrives, and dfps' TopappMonitor publishes only when the top-app task
count moves by more than `TOP_TASK_NR_DIFF_MIN` (10). So `top=None` is the normal
initial state, and a launch that does not change the count produces no event — which
is why the first attempt at this test saw `top=None` throughout: Settings was already
in the foreground from a previous run, so launching it again was a no-op. The fix was
to go home first.

## 3. Observability gap found while doing this (fixed) [V]

The context scheduler's periodic summary line was **never emitted in dry-run mode**:
the counters it is gated on (`affinity_changes`/`policy_changes`) only increment on
the non-dry path, so a `UPERF_SCHED_DRY_RUN=1` harness saw no summary at all — and
therefore no `scene=`/`top=` over time, which is exactly what §2 needed. There is now
a `would_change` counter incremented in the dry-run branch, included in the gate and
the line (`dry_would_change=`), so a dry run reports what it would have done.

## 4. Still open

* `modules.input.enable` is not honoured (see `docs/m6b-evidence.md` §11). **[U]**
* the `sfanalysis.hint` producer's path (see §8 of `m6b`). **[U]**
* `auto` in `cur_powermode.txt` **[I]**.
