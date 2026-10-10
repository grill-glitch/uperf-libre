# Unsafe audit — `uperf-libre` (`rust/` workspace + `tools/`)

Scope of this document: a source-driven audit of every `unsafe` occurrence in the
repository, the changes made, the evidence for them, and the calls that were
deliberately *not* made. It is written to be checkable: every claim below is
either **[V]** (verified with a tool in this session, command included), **[I]**
(inferred from code reading, argument given) or **[U]** (unknown / needs the
maintainer or a device).

Branch `game-turbo`, base commit `d40907347d4209a818bb4906f0f03c7094c6df91`.
Working tree was clean before the audit and contains no unrelated edits after it
(`git diff --stat`: 15 files, +252/−42, all in the files listed in §4).

---

## 1. Baseline

### 1.1 Repository shape

| Path | What it is | Rust `unsafe`? |
| --- | --- | --- |
| `rust/uperf-config` | shared config layer + sysfs planner (used by both the host tool and the device binary) | **none** |
| `rust/uperf-cli` | host-only parity tool | **none** |
| `rust/uperf-core` | the Android `staticlib` linked into the C++ supervisor — the bulk of the project | yes |
| `rust/uperf-sfanalysis` | `cdylib` injected into `surfaceflinger` (GOT/PLT rewriting + observer) | yes |
| `rust/uperf-core/src/dfps_rs/` | **git subtree** from `grill-glitch/dfps-rewrite` ("do not edit here") | none |
| `tools/{binder-probe,ctl-listen,uprobe-probe}` | three standalone device-harness crates (own `Cargo.lock`, not workspace members) | yes |
| `cpp/` | vendored `dfps` platform layer, C++ (Apache-2.0) | n/a (no Rust) |
| `helper/`, `magisk/`, `scripts/`, `webui/` | Java helper + shell/module packaging | n/a |

`dfps_rs/` is a subtree and was **not modified**; it also contains no `unsafe`
(`ls-files` count below), so the exclusion costs nothing.

### 1.2 Toolchain and targets

* `rustc 1.94.0-nightly (8d670b93d 2025-12-31)`, `cargo 1.94.0-nightly` **[V]**
* No `rust-toolchain.toml` / `rust-toolchain` anywhere in the repo **[V]** — the
  build follows the ambient default toolchain.
* Edition **2021** in all four crates **[V]** (`rust/Cargo.toml`, per-crate manifests).
* Real target: `aarch64-linux-android` (NDK `android-ndk-r30`, present). Host
  target `x86_64-unknown-linux-gnu` is used for the unit tests.
* Release profile: `opt-level="z"`, `lto="fat"`, `codegen-units=1`,
  `panic="abort"`, `strip=true`, `overflow-checks=true` **[V]**.
* CI: no `.github/` in the tree — there is no CI workflow to satisfy **[V]**.

### 1.3 Baseline check results (before any edit)

| Command | Result |
| --- | --- |
| `cargo check --workspace --all-targets` (host) | **pass**, 6 warnings |
| `cargo check --workspace --all-targets --target aarch64-linux-android` | **pass**, 2 warnings |
| `cargo test --workspace` (host) | **pass**, 265 tests, 0 failures |
| `cargo clippy --workspace --all-targets` (host) | **FAIL (exit 101)** |
| `cargo clippy … --target aarch64-linux-android` | **FAIL (exit 101)** |
| `cargo fmt --all -- --check` | **FAIL** — 326 diff hunks across **47 of 53** tracked `.rs` files |

Pre-existing failures, recorded so they are not mistaken for regressions:

1. **`cargo clippy` error** (not a warning):
   `error: this public function might dereference a raw pointer but is not marked
   unsafe` → `uperf-sfanalysis/src/observe.rs:162` (`clippy::not_unsafe_ptr_arg_deref`,
   `deny` by default). This is a legitimate finding, not a false positive: see §3, F-1.
2. **`cargo fmt --check` fails on 47 of 53 files.** The project is deliberately
   hand-formatted (`if x { y } else { z }` on one line, long single-line
   statements). `cargo fmt` was therefore **not** run — doing so would rewrite the
   whole repository and drown a 15-file safety diff. Formatting of the touched
   regions was kept consistent with the surrounding style by hand.
3. Because clippy aborted on the `deny` error inside `uperf-sfanalysis`, in the
   baseline `uperf-core` was **never** clippy-checked on either target. Its clippy
   warning set is reported for the first time in §7.

The compiler's `unused_unsafe` lint was already enabled by default and reported
**no** findings on either target at baseline **[V]** — i.e. there is no
"`unsafe` block containing nothing that needs it" anywhere. Category A of the task
brief is, for this repository, empty. That is the single most useful baseline fact
here, and it means a lowered occurrence count was never going to be the measure of
this audit.

---

## 2. Inventory and counting method

### 2.1 Method

Counting is done on **comment-stripped** source (a small tokenizer that skips
`//`/`/* */` and string/char literals) over every tracked `.rs` file outside
`*/target/`, counting the keyword `unsafe` in each of four syntactic positions:

* `block` — `unsafe { … }` (block or expression),
* `fn` — `unsafe fn` / `unsafe unsafe` declarations,
* `fnptr` — `unsafe extern "C" fn(…)` function-pointer types (e.g. fields of a
  `#[repr(C)]` vtable struct),
* `impl` — `unsafe impl` / `unsafe trait` (**zero** in this repository).

Notes on the method, both of which matter for the numbers:

* One line with two `unsafe` keywords in two *different* positions counts 2; a
  single `unsafe { a(); b(); }` counts 1 regardless of how many unsafe operations
  it contains. So this is a count of **annotation sites**, not of unsafe
  operations and not of unsafe *calls*.
* Comments are excluded (`// SAFETY: …` does not move the number), and so are the
  lint names `unused_unsafe`/`unsafe_code` (the `\b` boundary makes them
  non-matches) — otherwise this audit would appear to *add* `unsafe` by writing
  documentation about it.

### 2.2 Totals

| | block | fn | fnptr | impl | **total** |
| --- | --- | --- | --- | --- | --- |
| **before** | 137 | 5 | 3 | 0 | **145** |
| **after** | 143 | 4 | 6 | 0 | **153** |

**The total went up by 8, and no unsafe operation was added.** The delta, exactly:

| Δ | Where | Why |
| --- | --- | --- |
| +4 blocks | `sfanalysis/hook.rs` | explicit `unsafe { … }` now written *inside* the three remaining `unsafe fn`s (`read_insns`, `arch::probe`, `arch::flush_code`×2), required by the newly enabled `unsafe_op_in_unsafe_fn` lint. Same operations, now lexically visible. |
| +3 fnptr | `uperf-core/lib.rs` ×2, `sfanalysis/observe.rs` ×1 | the FFI entry points that already dereferenced a caller-supplied raw pointer are now declared `unsafe extern "C" fn` (F-1, F-6). ABI and symbol unchanged. |
| +2 blocks | `sfanalysis/observe.rs` (tests) | the two in-crate call sites of `sfh_stats`, which is now `unsafe fn`. |
| −1 fn | `sfanalysis/hook.rs` | the dead `pub unsafe fn read_words` deleted. |

This is precisely the case the brief warns about ("changing block boundaries can
alter counts without changing the underlying operations"). The honest headline is:
**one unsafe function removed, one unsoundness-adjacent public API made
explicitly unsafe, zero unsafe operations added, and every remaining occurrence
now carries an auditable justification.**

### 2.3 Per-file inventory and classification

Classification follows the brief (A unnecessary / B replaceable-safe /
C necessary-but-too-broad / D necessary-and-justified / E unresolved).

| File | before | after | classes present |
| --- | --- | --- | --- |
| `uperf-core/src/lib.rs` | 11 | 13 | C, D |
| `uperf-core/src/ffi.rs` | 5 | 5 | D |
| `uperf-core/src/topic_dispatch.rs` | 4 | 4 | D |
| `uperf-core/src/ctl_socket.rs` | 6 | 6 | C, D |
| `uperf-core/src/inotify.rs` | 7 | 7 | D |
| `uperf-core/src/sched_apply.rs` | 4 | 4 | C, D |
| `uperf-core/src/sf_binder.rs` | 18 | 18 | C, D, E→D |
| `uperf-core/src/sf_uprobe.rs` | 4 | 4 | D |
| `uperf-core/src/{cpu_task,sched_task,watch_task,recorder,foreground}.rs` | 1 each | 1 each | D |
| `uperf-core/src/startup_lines.rs` (test) | 1 | 1 | D |
| `uperf-core/tests/payload_decode.rs` | 1 | 1 | D |
| `uperf-sfanalysis/src/got.rs` | 16 | 16 | C, D |
| `uperf-sfanalysis/src/hook.rs` | 16 | 19 | A (removed), C, D |
| `uperf-sfanalysis/src/observe.rs` | 3 | 6 | A (fixed), D |
| `uperf-sfanalysis/src/lib.rs` | 1 | 1 | D |
| `tools/binder-probe/src/main.rs` | 17 | 17 | D |
| `tools/uprobe-probe/src/main.rs` | 17 | 17 | D |
| `tools/ctl-listen/src/main.rs` | 9 | 9 | D |
| `uperf-config`, `uperf-cli`, `dfps_rs` | **0** | **0** | — |
| **total** | **145** | **153** | |

No `static mut`, no `union`, no `unsafe impl`, no inline-`asm!` outside
`sfanalysis/hook.rs::flush_code`, no `transmute` anywhere **[V]**
(`grep -n "static mut\|union \|transmute\|unsafe impl" rust tools` → empty).

---

## 3. Findings

### A — unnecessary `unsafe` (removed)

**F-1 (was `deny`-level error). `sfanalysis::observe::sfh_stats` was a *safe*
`pub extern "C" fn` that wrote through a caller-supplied raw pointer.**
`out.add(i)` + `write_unaligned` behind a safe signature means a Rust caller can
trigger a wild write with no `unsafe` keyword in sight; that is what
`clippy::not_unsafe_ptr_arg_deref` was flagging. The signature is ABI, not
behaviour — `unsafe extern "C" fn` emits the *same* symbol and calling convention,
so the on-device harness (`dlsym` + C declaration) is unaffected **[I]**, while
Rust callers must now acknowledge the contract. Fixed: signature marked `unsafe`,
`# Safety` section added, the two in-crate test call sites wrapped.

**F-2 `sfanalysis::hook::read_words` — dead `pub unsafe fn`.**
`pub unsafe fn read_words(addr, n) -> Vec<u32> { read_insns(addr, n) }`, with zero
callers on either target **[V]** (`grep -rn read_words` → only the definition). An
unused `unsafe fn` is unsafe surface with no consumer: any future caller inherits
an obligation nobody has ever had to reason about. Deleted.

**F-3 `sfanalysis::hook` — `extern "C" { fn sysconf(name: i32) -> i64; }` declared and never used.**
The only `sysconf` call in the crate goes through `libc::sysconf` (line 188). The
hand-written declaration produced a `dead_code` warning on **both** targets at
baseline. Removed — one fewer hand-written FFI declaration to keep in sync with
bionic.

### B — replaceable with safe Rust

**F-4 candidate: `ctl_socket::connect` / `sockaddr` → `std::os::{linux,android}::net::SocketAddrExt`.**
The abstract-socket path is built by hand: `mem::zeroed::<sockaddr_un>()`, byte
copies into `sun_path`, `socket(2)`, `connect(2)`, `close(2)`, `from_raw_fd` — 5 of
that file's 6 annotation sites (the sixth is `uid()`'s `getuid`, which has nothing to
do with the socket and stays as it is). Since Rust 1.70 std can do all of it:
`SocketAddr::from_abstract_name(name)` + `UnixStream::connect_addr(&sa)`, and the
trait exists on the device target too — **verified by compile probe**:

```
std::os::linux::net::SocketAddrExt     -> host   (x86_64-unknown-linux-gnu)
std::os::android::net::SocketAddrExt   -> device (aarch64-linux-android)
```

**Not done, deliberately.** Three reasons, in order of weight:

1. The replacement cannot be *device*-verified from here, and this is a
   **device-verified, opt-in, security-relevant path** (the ⑦ control-plane
   handshake). The brief forbids changing externally observable behaviour, and the
   equivalence I can establish is static only.
2. It changes the error strings on two failure paths (`socket: …` disappears — std
   merges socket+connect), and `connect {addr}` error text is logged.
3. The existing test `ctl_socket::tests::abstract_and_path_addresses_are_both_accepted`
   asserts the raw `sockaddr_un` layout (`sun_path[0] == 0`, `len == 2+1+3`), so it
   would have to be rewritten — a test rewrite in a path I cannot then re-verify on
   hardware.

Recorded as follow-up **R-1** below, with the exact API and the compile evidence, so
the maintainer can land it behind a device round. This is the single largest
safe-replacement opportunity in the repository (6 → 0), which is why it is written
down rather than silently dropped.

**F-5 (checked, none found). Other B candidates.** `sf_binder::as_bytes` (byte view
of a POD struct) has no std equivalent without a new crate (`bytemuck`), which the
brief forbids; `hook::read_insns` / `observe::note` read memory at a kernel- or
caller-provided address (no safe equivalent); `inotify`/`sched_apply` are raw
`inotify_add_watch`/`sched_setaffinity` (std has no equivalent). **[I]**

### C — necessary, boundary too broad (narrowed)

**F-6 `uperf-core` FFI entry points dereferenced raw pointers while not declared `unsafe`.**
`uperf_rs_on_event` was already `unsafe extern "C" fn`; `uperf_rs_init(bridge: *const Bridge)`
and `uperf_rs_start(config_path, log_path)` were **not**, yet both immediately
dereference their arguments (`(*bridge).clone()`, `CStr::from_ptr`). A Rust-side
caller could invoke UB without an `unsafe` block. Both are now
`unsafe extern "C" fn` with a `# Safety` section. ABI, symbol and the C++ call sites
(`cpp/uperf/bridge.cpp:301-302`) are unchanged — this is a declaration, not a
transformation.

**F-7 `sched_apply::set_affinity` — validation inside the `unsafe` block.**
The `CPU_SETSIZE` bounds check lived *inside* the block, between `CPU_SET` calls,
so the block's own precondition ("every id written is in range") was established
half-way through it. The check now runs before the block, which is all that keeps
`CPU_SET` from writing past the end of the stack `cpu_set_t`. Same error, same
message, same order of observable effects (the check is pure).

**F-8 `sfanalysis::hook.rs` — unsafe operations implicit in `unsafe fn` bodies.**
`read_insns`, `arch::probe` and `arch::flush_code` performed unsafe operations
without an inner `unsafe` block. Edition-2024 semantics (and
`#![deny(unsafe_op_in_unsafe_fn)]`, which `uperf-core` already had and this crate
did not) make them explicit. No behaviour change; the point is that every unsafe
operation is now individually visible and individually justifiable.

**F-9 (documented, not structurally narrowed) — undocumented unsafe invariants.**
The audit's other real product: **the riskiest sites had no written argument for why
they are sound.** The two that mattered most:

* `sf_binder::parse` created a slice from `t.data_buffer`/`t.data_offsets` —
  **kernel-supplied pointers** — with no invariant stated anywhere. The argument
  (the kernel returns addresses inside the 256 KiB read-only binder mapping created
  in `Binder::open`, with `data_size`/`offsets_size` valid bytes, released by
  `BC_FREE_BUFFER`) is now written at the site.
* `sfanalysis/got.rs::cb` dereferences loader-owned (`dl_iterate_phdr`) data. The
  bounds discipline (`in_range`) and the reason each dereference is safe are now
  recorded, including *why* `read_unaligned` is required for GOT entries.

All retained sites now carry `// SAFETY:` comments stating the precondition and why
it holds (§5).

### D — necessary and adequately justified (retained)

libc/`extern "C"` calls with no safe equivalent, and their resource ownership:
`clock_gettime` ×3, `getuid`/`geteuid`/`gettid`, `sched_setscheduler`,
`setpriority`, `inotify_init1`/`inotify_add_watch`/`poll`/`read`, `pipe`/`mmap`/
`ioctl`/`close`/`munmap`, `socket`/`connect`, `perf_event_open` + perf-event
`ioctl`/`read`, `dl_iterate_phdr`, `dlsym`, `mprotect`, `signal`,
`FromRawFd`/`from_raw_parts`/`write_unaligned`/`read_unaligned`, and the aarch64
cache-maintenance `asm!`. Each is either a syscall with no std wrapper, an FFI
boundary whose contract comes from the C++ side, or a raw-memory operation whose
precondition is now documented.

### E — unresolved / potentially unsound (not silently resolved)

**F-10 `sfanalysis::hook::arch::install_inline` is dead code that is *kept on
purpose*.** It is `#[allow(dead_code)]`, never called (`install()` takes the GOT
route), and is retained with a comment saying it is "the reference for what an
inline patch would have to do". It carries ~10 annotation sites, including an
`mmap(PROT_READ|WRITE|EXEC)` trampoline, `mprotect(… RWX)` on a code page, and the
`dc cvau`/`ic ivau` flush helpers — i.e. the exact operations this device refuses
(`execmem`/`execmod`). It is unreachable, so it cannot be exploited, but it is the
one group of unsafe sites in the shipped crates with **no runtime justification**.

Not removed here, because deleting a deliberately-retained reference implementation
is the maintainer's call, not an auditor's (and the brief forbids unrelated
rewrites). **Resolved by the R-2 audit (§11): it is compiled for aarch64 but is not
retained in any artifact (LTO *and* `--gc-sections` drop it — proven byte-for-byte),
and it is unreachable. It is retained with that evidence written at the site.**

**F-11 the three `tools/*` harnesses are unaudited copies of the transport code.**
`tools/binder-probe/src/main.rs` (17), `uprobe-probe` (17), `ctl-listen` (9) are
near-duplicates of `sf_binder.rs` / `sf_uprobe.rs` / a server-side `ctl_socket`.
Their unsafe is the same category D, but they are **separate crates with their own
`Cargo.lock`**, outside the workspace, so `cargo check --workspace` never sees them
and no lint added to the workspace crates protects them. They are device harnesses,
not shipped artifacts. Either de-duplicate them onto the library code or accept the
divergence explicitly; see **R-3**. (Untouched by this change set.)

**F-12 no unsafe abstraction in this repo is `Send`/`Sync` by construction.**
`Binder` in `sf_binder.rs` owns a raw `map: *mut u8`; it is used from one thread and
never crosses one, so there is no unsoundness *today* — but a raw pointer field
makes `Binder` `!Send`/`!Sync` implicitly, and that is the only thing preventing a
second thread from using it. That is an accident of the type system rather than a
stated invariant. No change made (it would be a design change); recorded so the
next person who wants to share it knows the current safety rests on
`*mut u8` being non-`Send`. **[I]**

---

## 4. Source changes

15 files, +252 / −42. Nothing outside `rust/`.

| # | File | Change |
| --- | --- | --- |
| 1 | `uperf-sfanalysis/src/observe.rs` | **F-1**: `sfh_stats` → `pub unsafe extern "C" fn` + `# Safety`; 2 test call sites wrapped; `note()`'s `binder_write_read` read documented |
| 2 | `uperf-sfanalysis/src/hook.rs` | **F-2** deleted `read_words`; **F-3** deleted unused `sysconf` declaration; **F-8** explicit blocks in `read_insns` / `arch::probe` / `arch::flush_code`; `// SAFETY:` on `errno`, `page_size` |
| 3 | `uperf-sfanalysis/src/got.rs` | **F-9**: `// SAFETY:` on `find_slots`, the whole `cb` walk (loader data, `in_range` discipline, `dlpi_name`, reloc/sym/strtab reads, GOT read) and `patch_slot`'s two `mprotect`s + slot write |
| 4 | `uperf-sfanalysis/src/lib.rs` | **F-8**: `#![deny(unsafe_op_in_unsafe_fn)]` + `#![deny(unused_unsafe)]` |
| 5 | `uperf-core/src/lib.rs` | **F-6**: `uperf_rs_init`, `uperf_rs_start` → `unsafe extern "C" fn` + `# Safety`; `#![deny(unused_unsafe)]` (pairs with the existing `deny(unsafe_op_in_unsafe_fn)`) |
| 6 | `uperf-core/src/sched_apply.rs` | **F-7**: `CPU_SETSIZE` validation hoisted out of the `unsafe` block |
| 7 | `uperf-core/src/sf_binder.rs` | **F-5/F-9**: `as_bytes` precondition written; three `const _: () = assert!(size_of…)` pinning the layout `as_bytes`/`parse`/`get_service` depend on; `// SAFETY:` on `open`/`mmap`/`Drop`/`bwr`/`parse`(×3)/`get_service`/`dump`(×4)/`connect`/`monotonic_ns` |
| 8 | `uperf-core/src/sf_uprobe.rs` | `// SAFETY:` on `perf_event_open`, `FromRawFd`, perf `ioctl`, perf `read` |
| 9 | `uperf-core/src/inotify.rs` | `// SAFETY:` on `inotify_init1`, both `inotify_add_watch` calls |
| 10 | `uperf-core/src/ctl_socket.rs` | **F-9**: `// SAFETY:` on `sockaddr`'s `zeroed`, `socket`, `connect`, `close`, `from_raw_fd`, `getuid` |
| 11 | `uperf-core/src/recorder.rs` | `// SAFETY:` on `clock_gettime` |
| 12 | `uperf-core/src/startup_lines.rs` | `// SAFETY:` on the test's `geteuid` |
| 13 | `uperf-config/src/lib.rs` | `#![forbid(unsafe_code)]` (crate has zero `unsafe`) |
| 14 | `uperf-cli/src/lib.rs`, `uperf-cli/src/main.rs` | `#![forbid(unsafe_code)]` (crate has zero `unsafe`) |

Not touched: `dfps_rs/` (subtree), `tools/*` (F-11), `cpp/*`, configs, docs, scripts.

### 4.1 The three `const _: () = assert!(…)` additions

These are the only *new* safety machinery, and they pin an invariant that the unsafe
code already relied on silently: `parse` advances `TXN_SIZE` bytes per transaction
so `BinderTransactionData` **must** be exactly that size; `FlatBinderObject` is read
with a literal `o + 24 <= data.len()` bound; `BinderWriteRead` is the ioctl's
argument and its size is part of the `BINDER_WRITE_READ` request encoding. A field
added or reordered would previously have produced a silently corrupt binder command
stream; it is now a compile error.

---

## 5. Safety rationale for every retained `unsafe` site

Grouped by invariant, since the sites share one argument. All are now in the source
as `// SAFETY:` comments; this is the summary.

| Group | Sites | Why it is sound |
| --- | --- | --- |
| **C++ bridge FFI** | `uperf_rs_on_event`, `uperf_rs_init`, `uperf_rs_start`, `uperf_rs_reload`, `Bridge::subscribe`, `LogLine::msg`, `uperf_bridge_write_log` ×5 callers, `uperf_bridge_set_*` ×2 | Contract is the C side's: `cpp/include/uperf_rs.h` §5.1 — `topic` NUL-terminated UTF-8, `data` valid only for the call (copied before use), `Bridge*` process-lifetime. Every pointer-taking entry is now `unsafe`, so the obligation cannot be invoked accidentally. |
| **libc out-parameters** | `clock_gettime` ×3, `sched_setaffinity`, `sched_setscheduler`, `setpriority`, `poll`, `read` (inotify/perf/binder), `inotify_add_watch` ×2, `open`, `mmap`, `ioctl` ×4, `pipe`, `socket`, `connect`, `munmap`, `close` ×6 | Each argument is a live local (or a `Vec` that outlives the call) of exactly the size the syscall writes; results are checked, never assumed. |
| **Resource ownership** | `FromRawFd`/`from_raw_fd` ×4, `Drop for Binder`, `Drop for Inotify` | Each raw fd is created in the same function and handed to exactly one owner (`File`/`UnixStream`/the struct), which closes it once; the failure paths close before returning. |
| **Raw memory we control** | `read_insns`, `observe::note`'s `binder_write_read` read, `sfh_stats`'s writes, `sf_binder::as_bytes` + `parse` + `get_service` | Bounds are checked immediately before the read/write (`off + TXN_SIZE <= len`, `i < m <= n`, `o + 24 <= data.len()`, `reply.len() >= 28`); alignment is never assumed (`read_unaligned`/`write_unaligned`). |
| **Memory the kernel fills** | `sf_binder::parse`'s two `from_raw_parts` | Address comes from the kernel in a `BR_REPLY`/`BR_TRANSACTION` and points inside the mapping made in `Binder::open` (256 KiB, `PROT_READ`); the copy is taken immediately and the buffer released with `BC_FREE_BUFFER`. Now stated at the site. |
| **Memory the loader owns** | all of `got.rs::cb` | `dl_iterate_phdr` hands each module's `dl_phdr_info`; `dlpi_phdr` has `dlpi_phnum` entries; every dynamic-table/reloc/sym/strtab address is gated by `in_range` before use (this is the device-verified SIGSEGV fix in `xh_refresh_loop`). |
| **`surfaceflinger`'s GOT** | `patch_slot` | `addr` is `load bias + r_offset` for a relocation the target module owns, so it is inside that module's RELRO/GOT; the page is masked to a page boundary and `mprotect`ed RW (a *data* page — no `execmem`/`execmod`), restored afterwards. |
| **aarch64 cache/ASM** | `flush_code`, `arch_shim_for`, `global_asm!` | Cache maintenance by VA on a mapping the library owns; the shims save/restore `x19`–`x22`, `x29`, `x30` around the tail call and reach the original through the `AtomicUsize` slots. |
| **Scheduler syscalls** | `sched_apply.rs` | `cpu_set_t` ids validated against `CPU_SETSIZE` *before* the block (F-7); `sched_param` outlives the call. |

---

## 6. Test changes and coverage

* `sfanalysis/observe.rs`: two call sites of `sfh_stats` wrapped in `unsafe { … }`
  because the function is now `unsafe`. **No assertion was weakened, no test
  disabled, no `#[ignore]` added** — the suite is unchanged in content.
* No new test was added. Rationale: the change set alters *declarations*,
  *comments*, one dead-code deletion and one hoisted check — there is no new
  behaviour to assert. The pre-existing tests already pin the behaviour the
  modified code has: `hook::tests::pc_relative_detection`,
  `device_prologues_are_relocation_safe`, `observe::tests::*` (10),
  `sf_binder::tests::*` (10, including `parse_latency` / `count`/`frame_hint`),
  `ctl_socket::tests::*` (5), `sched_apply`'s device-syscall tests, plus the
  63-config integration tests (**265 total**).
* The one test that *would* have caught the F-1 class of mistake is the compiler:
  `clippy::not_unsafe_ptr_arg_deref` is now clean (§7).

---

## 7. Validation

| Command | Before | After |
| --- | --- | --- |
| `cargo check --workspace --all-targets` (host) | pass | **pass**, same 2 pre-existing `sfanalysis` warnings |
| `cargo check --workspace --all-targets --target aarch64-linux-android` | pass | **pass**, 0 warnings in the workspace crates |
| `cargo test --workspace` (host) | 265 passed | **265 passed, 0 failed** |
| `cargo clippy --workspace --all-targets` (host) | **exit 101**, 1 `deny` error | **exit 0** (no errors) |
| `cargo clippy --workspace --all-targets --target aarch64-linux-android` | **exit 101**, 1 `deny` error | **exit 0** (no errors) |
| `cargo build -p uperf-core --release --target aarch64-linux-android` | — | **pass** (`libuperf_core.a` produced) |
| `cargo build -p uperf-sfanalysis --release --target aarch64-linux-android` | — | **pass** (`libsfanalysis_rs.so` produced) |
| `sh build.sh Release make` (full C++ + NDK link) | — | **exit 0** (§7.1) |
| `cargo fmt --all -- --check` | FAIL, 326 hunks / 47 files | **unchanged** — not run, see §1.3(2) |
| `cargo miri test …` | — | **not run**, see §7.2 |

`#![deny(unused_unsafe)]` is now in force in both crates that contain `unsafe`, and
`#![deny(unsafe_op_in_unsafe_fn)]` in both. Both are enforced by the compiler
rather than by a script, so the property survives future edits.

### 7.1 Full C++ + NDK link — run, passed

`sh build.sh Release make` (with `ANDROID_NDK=…/android-ndk-r30`) was run after the
change set. Result: **exit 0**, `[100%] Built target uperf`, C++ supervisor linked
against the modified Rust `staticlib`, no compiler/linker errors and no warnings
other than a pre-existing CMake `cmake_minimum_required` deprecation from the
vendored `scnlib`. This is the real ABI check for F-6: `uperf_rs_init` /
`uperf_rs_start` are still resolved by `cpp/uperf/bridge.cpp` after being declared
`unsafe extern "C"`.

`build.sh pack` was deliberately **not** run: it rewrites the checked-in
`magisk/bin/uperf` (a 2.3 MB binary), which would put a blob unrelated to the audit
into this change set. Consequently `build.sh check` — which asserts the freshly
built binary is byte-identical to the checked-in `magisk/bin/uperf` — would fail
here, as it must after any source change until `pack` re-stages: the current build
is `5e853cc8…` while the checked-in artifact is `d27d2e59…` **[V]**. That is the
expected state for a source-only change set, not a regression.

### 7.2 Not run: Miri

`cargo miri` is **not installed for the active toolchain**
(`nightly-2026-01-01-x86_64-unknown-linux-gnu`); `rustup run miri …` and
`cargo +miri` both fail with `toolchain 'miri' is not installed` **[V]**. Installing
the component would modify the pinned toolchain, which the brief forbids. Even if
installed, most of the unsafe here is incompatible with Miri by construction
(`dlsym`, `dl_iterate_phdr`, `/proc` reads, `perf_event_open`, binder `ioctl`,
`mprotect` on foreign pages), so a Miri run could not have covered the sites that
matter — the closest applicable path would be `sfanalysis::observe` (atomics plus the
`sfh_stats` raw write). **No claim of memory safety is made on the basis of Miri;
none was run.**

### 7.3 Not run: on-device verification

No device round was performed in this session. Nothing in this change set alters
runtime behaviour by construction (declarations, comments, a hoisted pure check, a
deleted dead function, three compile-time assertions), but that is an argument, not
a measurement — see §9.

---

## 8. Performance and behaviour impact

* **No code added to any hot path.** The changes are: comments; `#![…]` lint
  attributes; `unsafe`/`unsafe extern` markers on function declarations (zero code
  generation); one deleted dead function (smaller binary); the `CPU_SETSIZE` check
  hoisted out of a block that is called per thread-application, replacing an
  identical check (same comparison count, same order, no allocation added — the
  `format!` for the error string now happens before entering the block, and only on
  the error path, which previously *also* allocated inside the block); and three
  `const` assertions, which are evaluated at compile time and emit nothing.
* The only observable-behaviour claim is that `assert!(!bridge.is_null())` in
  `uperf_rs_init` is still present (an unchanged diagnostic) and that no error
  string changed. Both are visible in the diff. **[V]**
* `sfh_stats`'s ABI is unchanged (same `#[no_mangle]` symbol, same
  `(u64*, usize) -> usize` C signature); only the Rust-side declaration gained
  `unsafe`. Any caller reaching it by symbol is unaffected. **[I]**
* Release artifacts rebuild: `libuperf_core.a` (13.9 MB) and `libsfanalysis_rs.so`
  (300 KB) for `aarch64-linux-android` **[V]**. Staticlib byte size is not a
  behaviour signal and is not claimed to be unchanged.

---

## 9. Remaining risks and follow-ups

**R-1 (highest value). Replace `ctl_socket`'s hand-built address + raw socket with
std.** `SocketAddr::from_abstract_name` + `UnixStream::connect_addr`, with
`#[cfg(target_os = "android")] use std::os::android::net::SocketAddrExt;` and
`#[cfg(target_os = "linux")] use std::os::linux::net::SocketAddrExt;` — both paths
compile (probe above). Removes 5 of the 145 sites. **Implemented and device-validated
on alioth — see §10.**

**R-2. `hook::arch::install_inline`** (F-10) — audited in **§11**: it is *compiled* for
aarch64 but **not retained** in the artifact in either the release or the dev build,
and it is unreachable. Retained with its `// SAFETY:`-style evidence at the site
(documentation only; the cdylib is byte-identical before and after).

**R-3. The three `tools/*` harnesses duplicate the transport code** (F-11) and are
outside the workspace, so no workspace lint protects them. Either point them at the
library crates or add them to a workspace so `deny(unused_unsafe)` /
`deny(unsafe_op_in_unsafe_fn)` reach them.

**R-4. `as_bytes<T>` is an unconstrained generic** whose contract is "T is POD with
no padding". Its three call sites are safe today, and the size assertions pin the
*size*; nothing pins the absence of *padding*. Adding `#[repr(C)]`-only types or a
`bytemuck::Pod`-style bound would be the structural fix, but the brief forbids new
dependencies, so it is recorded rather than done.

**R-5. `Binder`'s `*mut u8` map field** (F-12) makes the type implicitly `!Send`;
that is currently the only reason the mapping is not shared across threads.

**R-6. No `unsafe` in this change set was validated on hardware.** The device-verified
claims in this document (GOT rewriting, the binder transport, the ctl-socket
handshake, the ⑪ uprobe leg) are *pre-existing* facts about the code, taken from the
repository's own docs and the maintainer's earlier rounds — not from a run made in
this session.

**Explicitly not claimed:** that the remaining annotation sites are free of
undefined behaviour. What is claimed is that each one now has a written
precondition, that the compiler checks the ones a compiler can check
(`unused_unsafe`, `unsafe_op_in_unsafe_fn`, `not_unsafe_ptr_arg_deref`), that one
dead `unsafe fn` is gone, that one unsafe-in-a-safe-signature defect is fixed, that
R-1 (§10) removed five more, and that nothing else changed.

---

# 10. R-1 follow-up — safe Unix abstract socket, device-validated (2026-10-10)

**Outcome: implemented, and validated on the real alioth device.** The hand-built
`sockaddr_un` + raw `socket`/`connect`/`close`/`from_raw_fd` path in `ctl_socket` was
replaced by `std::os::{android,linux}::net::SocketAddrExt` +
`UnixStream::connect_addr`, and the required runtime tests were run on alioth against
the project's own independent, hand-built peer (`tools/ctl-listen`).

## 10.1 Device and repository state

| | |
| --- | --- |
| Serial / model | `f748d277` — `product:alioth`, `device:alioth`, `model:M2012K11AC` |
| Android | 16 (`ro.build.version.release`), SDK 36, `arm64-v8a` |
| Vendor fingerprint | `Redmi/alioth/alioth:13/TKQ1.221114.001/V816.0.6.0.TKHCNXM:user/release-keys` |
| Root | KernelSU: `su -c id` → `uid=0(root) … context=u:r:ksu:s0` |
| Installed module | `/data/adb/modules/uperf`, `bin/uperf` md5 `218ec07c11031346662d252ac7538a61`, 2 260 984 B, mtime 2026-10-10 13:33 |
| Installed daemon | pids `12981` (supervisor) + `12982` (worker) = the module binary; watchdog `13186`; `uperf.state` = `state=running takeover=off armed=0 pid=12982` |
| Repository | branch `game-turbo`, HEAD `d40907347d4209a818bb4906f0f03c7094c6df91`, working tree = the M12 audit edits + this refactor (16 entries) |

**The installed module binary is NOT the binary under test, and cannot be tied to a
source revision.** Its md5 matches no build produced here, and it predates this work
(13:33). That is *by design* for this validation and is stated plainly rather than
papered over: `tools/ctl-listen/e2e-ctl.sh` — the project's own documented ⑦ e2e
procedure — deliberately runs a **pushed** binary (`/data/local/tmp/uperf_ctl`)
against a pushed peer, so the install is neither used nor touched. The installed
daemon was alive before, during and after (same pids), and `/data/adb/modules/uperf`
still has every file at its original 13:33 mtime.

The artifact actually tested is therefore pinned by hash, not by install:

| | |
| --- | --- |
| Binary under test | `build/aarch64-linux-android23/runnable/uperf` |
| SHA-256 | `36464cbf5c75134ca7cad811a01d0b9e7490274c9085f5f9b7a655b867a43503` |
| md5 | `ad025263ce40c978ff537b5bf4a0a3e1` (2 367 192 B, built 22:17:55) |
| Source | HEAD `d409073…` + the working tree above; `ctl_socket.rs` mtime 22:17:14 (source precedes the binary) |
| Build config | `sh build.sh Release make`, `ANDROID_NDK=…/android-ndk-r30`, target `aarch64-linux-android23`, release profile (`lto=fat`, `panic=abort`, `strip`) |
| Peer under test | `tools/ctl-listen/target/aarch64-linux-android/release/ctl-listen`, SHA-256 `90da3116134723aaf441600c22f4fa81ac9fedd63375be32ce118699a2da8e83` (source unchanged by this work) |
| Hash association | md5 of `/data/local/tmp/uperf_ctl` on device = `ad025263…`; md5 of `/data/local/tmp/ctl-listen` = `3c0c5efb…` = the local peer's md5 — the bytes tested are the bytes built |

## 10.2 Old and new socket contracts

| Property | Old (hand-built) | New (std) | Same? |
| --- | --- | --- | --- |
| `@name` encoding | `sun_path[0]=0`, then the name bytes | `SocketAddr::from_abstract_name(name.bytes)` | **yes** — proven on device, not by reading std |
| Abstract `addrlen` | `sizeof(sa_family_t) + 1 + len` | std's `set_length(…+1+len)` | yes |
| Path encoding | bytes + the zeroed tail (NUL-terminated) | `SocketAddr::from_pathname(path)` | yes |
| Length limits | name ≤ 107 B, path ≤ 107 B refused above | identical boundaries (test-pinned) | yes |
| Namespace | abstract vs filesystem, chosen by the `@` prefix | same prefix rule | yes |
| Socket type | `AF_UNIX`, `SOCK_STREAM \| SOCK_CLOEXEC`, blocking | std uses `SOCK_CLOEXEC` on unix | yes (test-pinned via `/proc/self/fdinfo`) |
| fd ownership | `UnixStream::from_raw_fd` (closed once, on drop) | `UnixStream` from `connect_addr` | yes |
| Failure cleanup | `close(fd)` before returning the error | std closes internally | yes |
| Concurrency | none (one ctl thread; no shared state) | unchanged | yes |
| Public API | `pub fn connect(&str) -> Result<UnixStream, String>` | **unchanged** | yes — the signature is byte-identical |
| Protocol | `HELLO` / `OK v=` / `ERR` / `PING` / `PONG` | untouched | yes |

## 10.3 Unsafe operations removed

Counted with §2.1's method (comment-stripped, `unsafe` in a syntactic position). All
five were in `rust/uperf-core/src/ctl_socket.rs`:

| # | Old line / symbol | Operation | Now |
| --- | --- | --- | --- |
| 1 | `sockaddr` L161 | `mem::zeroed::<libc::sockaddr_un>()` | `SocketAddr::from_abstract_name` / `from_pathname` |
| 2 | `connect` L190 | `libc::socket(AF_UNIX, SOCK_STREAM\|SOCK_CLOEXEC, 0)` | inside `UnixStream::connect_addr` |
| 3 | `connect` L194 | `libc::connect(fd, &sa as *const sockaddr, len)` | inside `UnixStream::connect_addr` |
| 4 | `connect` L197 | `libc::close(fd)` on the failure path | std's own cleanup |
| 5 | `connect` L200 | `UnixStream::from_raw_fd(fd)` | `UnixStream` returned by std |

`uid()`'s `unsafe { libc::getuid() }` (L205 → L243) is **not** part of the socket path
and is retained; `std` has no `getuid`. No new `unsafe` was introduced anywhere (the
new tests use `AsRawFd`, `/proc` reads and std networking only).

Totals: §2.2's 153 → **148** (block 143 → 138; `unsafe fn` 4; `unsafe extern "C"`
fn-pointer types 6 — unchanged). Relative to the pre-audit baseline of 145, the net is
**+3**, entirely from the lint/declaration changes in §4: +4 explicit blocks inside
`unsafe fn`s, +3 `unsafe extern` markers, +2 test call sites of `sfh_stats`, −1 dead
`unsafe fn`, −5 here.

## 10.4 Error reporting

`UnixStream::connect_addr` performs `socket(2)` and `connect(2)` internally and
returns **one** `io::Error`, where the old code logged `socket: <e>` for the first and
`connect <addr>: <e>` for the second. The distinction is preserved rather than
silently collapsed:

* `sockaddr()` keeps the two **verbatim** validation messages —
  `abstract socket name too long: "<addr>"` and `socket path too long: "<addr>"`;
* `is_socket_creation_errno()` classifies the merged error as a `socket:` failure iff
  the errno is one only `socket(2)` can produce (`EMFILE`, `ENFILE`, `ENOMEM`,
  `ENOBUFS`, `EPROTONOSUPPORT`, `EAFNOSUPPORT`). `connect(2)` on an already-valid
  `AF_UNIX`/`SOCK_STREAM` fd cannot return any of them, so for every errno that can
  actually occur the original distinction is exact. `EACCES` is deliberately excluded:
  `connect(2)` does return it for a filesystem path, and the old code called that a
  `connect` failure;
* everything else keeps the `connect <addr>: <e>` form, as the device log confirms:
  `22:20:15 I Rust: ctl-socket: connect @uperf-r1fd: Connection refused (os error 111) (retrying)`.

Residual (documented, not hidden): a *hypothetical* `connect(2)` failure carrying one
of those six errnos would be labelled `socket:` — an under-report, never a false
success. No caller sees the error type: the only consumer is the one log line in
`CtlTask::spawn` (`Rust: ctl-socket: {e} (retrying)`), so no API or protocol changed.

**One real behaviour difference was found by a new test and then removed.** std's
`from_abstract_name(b"")` *succeeds* and builds the anonymous abstract address
(single NUL). The old code rejected `@` ("abstract socket name too long"), and
`UPERF_CTL_SOCKET=@` is reachable. Accepting it would have turned a config mistake
into an endless `ECONNREFUSED` retry loop instead of a one-line validation error, so
`sockaddr()` keeps an explicit empty-name rejection with the original message.
(Second, unreachable difference: std refuses a pathname with an interior NUL and
reports it with the "too long" message. An environment variable cannot contain a NUL,
so `UPERF_CTL_SOCKET` can never reach it.)

## 10.5 Tests

`ctl_socket`'s suite is 8 tests (was 5). The one deleted test asserted an internal
representation; four now assert observable behaviour:

| Test | Covers |
| --- | --- |
| `abstract_addresses_keep_the_exact_name_bytes` **(new, replaces `abstract_and_path_addresses_are_both_accepted`)** | the name bytes survive a round trip unchanged, an abstract address is not a pathname and vice versa — the property the old test got at by reading `sun_path[0]`/`sun_path[1]` |
| `address_length_limits_are_unchanged` **(new)** | the 107/108-byte boundary for both forms and the empty-name rejection with its exact message — the limits the refactor could silently have moved |
| `a_missing_endpoint_reports_a_connect_failure` **(new)** | §10.4: a missing path and a missing abstract name both produce `connect <addr>: …` |
| `abstract_connect_works_and_the_fd_is_cloexec` **(new)** | a real connect over an abstract address, `O_CLOEXEC` read from `/proc/self/fdinfo`, and a `PING`/`PONG` byte round trip — the two properties the deleted `unsafe` provided by hand |
| the other 4 (unchanged) | `HELLO` round trip, strict field/reply parsing, token 32-hex + `0600` + read-back, handshake against an owned listener (accept / version mismatch / `ERR`) |

Suite total: **265 → 268** (5 − 1 + 4 in `ctl_socket`). Nothing was weakened or
disabled.

## 10.6 Static validation (re-run after the refactor)

| Command | Result |
| --- | --- |
| `cargo check --workspace --all-targets` (host) | **pass, 0 errors** (2 pre-existing `sfanalysis` warnings) |
| `cargo check --workspace --all-targets --target aarch64-linux-android` | **pass, 0 errors** |
| `cargo test --workspace` (host) | **268 passed, 0 failed** (was 265) |
| `cargo clippy --workspace --all-targets` (host) | **0 errors** |
| `cargo clippy --workspace --all-targets --target aarch64-linux-android` | **0 errors** |
| `sh build.sh Release make` (C++ + NDK r30 link) | **exit 0**, `[100%] Built target uperf` |
| `cargo fmt --all -- --check` | not run — pre-existing failure on 47/53 files (§1.3) |

The two `sfanalysis` warnings and the profile warning are the pre-existing ones from
§1.3; no new warning appeared. `#![deny(unused_unsafe)]` and
`#![deny(unsafe_op_in_unsafe_fn)]` are in force in both crates that contain `unsafe`,
so this refactor could not have left a now-unnecessary block behind.

## 10.7 Runtime validation on alioth

Method: the project's own `tools/ctl-listen/e2e-ctl.sh` plus two supplementary harnesses
(`/data/local/tmp/ctl-r1-extra.sh`, `-fd.sh`, `-link.sh`), all pushed to
`/data/local/tmp` and run with `su -c 'sh …'`. The daemon under test always ran with
`UPERF_FAKE_ROOT=` + `UPERF_SCHED_DRY_RUN=1`, so **no sysfs write and no scheduling
call ever reached the real device**; the CPU governor was never armed.

**a) The handshake, against the independent hand-built peer** (`@uperf-e2e`):

```text
--- peer [accept] (reject=0) ---        ctl-listen: bound @uperf-e2e (reject=false, 14s)
peercred: uid=0 pid=8466               hello: HELLO v=1 pid=8466 uid=0 token=b491af0b…
hello_well_formed=true                 replied: OK v=1            pong
--- daemon [accept] ---                22:18:07 I Rust: ctl-socket: handshake ok, peer v1
                                       22:18:09 I Rust: ctl-socket: pong #1
```

This is the byte-fidelity evidence §3.1 asks for, and it is stronger than a unit test:
the peer binds the address by hand (`zeroed` + `sun_path[0]=0` + name + `2+1+len`) and
the new daemon connects with std's encoding. Any prefix, stray terminator or wrong
`addrlen` would have failed to match and no handshake could have happened.

**b) Refusing peer** → `ERR` answered, daemon logs
`ctl-socket: handshake failed: peer refused the handshake` and backs off; timestamps
`22:18:17 / :19 / :21` = exactly the configured `UPERF_CTL_RETRY_MS=2000`.

**c) Endpoint unavailable** → after the peer exits, the daemon logs
`ctl-socket: connect @uperf-e2e: Connection refused (os error 111) (retrying)` —
the §10.4 error form, produced by the *new* code, on device.

**d) Repeated connect/disconnect, no descriptor leak** — `/proc/<pid>/fd` sampled live:

| Run | Retry | Cycles observed | fd table |
| --- | --- | --- | --- |
| A (refusing peer, 11 s) | 1000 ms | 17 handshake failures, peer accepted **17** connections | `pid8670: fds=17` in **all five** samples (and `pid8669: fds=5` in all five) |
| fd harness (refusing peer, ~8 s) | 200 ms | **74** handshake failures, peer accepted **74** connections | 40 samples: `max_fds=18` (the series for run A is the flat part) |

No growth in either run. The worker's fd listing in run (e) also shows `pipe:` /
`anon_inode:inotify` / `/dev/input/event*` — the expected daemon furniture, with no
accumulating sockets.

**e) The link is a real, sustained socket** (accepting peer, 14 s, `UPERF_CTL_PING_MS=1000`):
`pid9549: fds=20/19/19/21/19/19/20/19, sockets=2` for all eight 1-second samples, peer
saw **1** connection and answered **11** PINGs, daemon logged `handshake ok, peer v1` +
`pong #1`. (The `sockets=2` are the ctl socket plus the platform layer's own socketpair;
the counter is only meaningful as ">0", which is the point. In the refusing runs the
socket lives for well under a millisecond per cycle — the peer answers instantly — which
is why the sampling there reports `sockets=0`; the connections are still proven by the
peer's 17/74 accepts. [I] for the lifetime estimate; [V] for the accept counts.)

**f) Restart and reconnection** — B1: `handshake ok, peer v1` + `pong #1`; `kill -TERM`
→ `22:19:25 I Rust: uperf_rs_stop: dispatcher joined` (graceful, governor disarmed at
the source level); B2, a second daemon against a second peer: `handshake ok, peer v1` +
`pong #1`. Two clean start/handshake/stop cycles.

**g) No hangs, crashes or leftovers** — every run ended with the test pids gone and the
installed daemon untouched. In the first e2e run the harness's own 2-second teardown
grace expired before one daemon finished exiting, so its final line still listed it;
the process was gone seconds later and its log shows the normal
`ctl-socket stopped` + `uperf_rs_stop: dispatcher joined`. The later runs report an
empty leftover list. Token file: `-rw------- 33` bytes (`0600` preserved).

## 10.8 Frequency-control and behaviour regression check

* **Governor state identical before and after the whole session** (read, never
  written): `policy0/4/7 → schedutil` both times — i.e. no `userspace` takeover was
  ever armed. `scaling_cur_freq` before: `1804800 / 710400 / 844800`.
* The refactor touches only the ctl address/connect code path; `UPERF_CTL_SOCKET` is
  opt-in and unset in the installed configuration, so the installed daemon's behaviour
  is unaffected by construction. The installed daemon's own state file still reads
  `state=running … pid=12982` with the same pids as before the session.
* No new allocation, lock, syscall, retry or poll was added: `connect_addr` performs
  the same two syscalls the old code did, and the errno classification is a pure
  branch. No performance claim is made — the existing workflow (a handshake plus a
  2-second keep-alive) cannot measure a difference between two implementations of one
  `connect(2)`, and no benchmark would be meaningful, so none is claimed.
* The extra `sockaddr()` branch (empty-name rejection) and the `map_err` closure add
  nothing to the hot path: they run once per connection attempt, which is already a
  syscall round trip.

## 10.9 Limitation and remaining risks

1. **The installed module build was not replaced, so the installed artifact is still
   the old implementation** and its exact source revision is unknown (md5
   `218ec07c…`). The validated binary is the pushed one. Deploying this change to the
   module remains a separate, deliberate step (`build.sh pack` + module install), and
   `build.sh check`'s binary-identity assertion will fail until that is done.
2. **The errno classification in §10.4 is a documented approximation** for one
   unreachable-in-practice class (a `connect(2)` failure returning one of the six
   `socket(2)` errnos would be labelled `socket:`). It cannot cause a false success.
3. **Only root (`u:r:ksu:s0`) to root was tested.** Both ends ran as root in the `ksu`
   domain, so this says nothing about SELinux behaviour when a *different* domain (an
   app, a `platform_app`, or an untrusted app) is the peer — the same limitation the
   original ⑦ verification recorded. The address/connect mechanism is unchanged in
   that respect, but the old code's explicit `SOCK_CLOEXEC` is now std's, which is
   verified here (host test + fdinfo) rather than assumed.
4. **The `@`-empty decision is a deliberate deviation from std's behaviour**, kept to
   preserve the previous observable behaviour. If the maintainer would rather accept
   the anonymous abstract address, deleting the two-line guard is the whole change.
5. `uid()`'s `getuid` (the 6th site in that file) is untouched: std has no equivalent
   and the call takes no arguments. Not R-1's business.
6. No Miri run, for the reasons in §7.2 — the refactor's remaining code is std, whose
   correctness is not this repository's to prove.

## 10.10 Deliverable summary for R-1

| | |
| --- | --- |
| Implemented? | **yes** — `rust/uperf-core/src/ctl_socket.rs`, safe std APIs |
| Unsafe eliminated | **5**, all in `ctl_socket.rs`: `sockaddr` L161 (`mem::zeroed::<sockaddr_un>`), `connect` L190 (`libc::socket`), L194 (`libc::connect`), L197 (`libc::close`), L200 (`UnixStream::from_raw_fd`) |
| Host checks | check / test (268) / clippy — **all pass, 0 errors** |
| Android checks | `cargo check` + `clippy` for `aarch64-linux-android` pass; `build.sh Release make` links the C++ binary (exit 0) |
| Real-device tests | **pass** — handshake against the hand-built peer, `ERR` refusal + 2 s backoff, `ECONNREFUSED` path, 17/74 connect-disconnect cycles with a flat fd table, sustained link with 11 PINGs answered, restart + reconnect, clean teardown |
| Rollback needed? | **no** — nothing outside `/data/local/tmp` was written; the installed module and daemon were untouched, and the governor was already `schedutil` and stayed there |
| Outstanding | deploy to the module is a separate step (10.9.1); the errno classification is a documented approximation (10.9.2); only root↔root was tested (10.9.3) |

---

# 11. R-2 follow-up — the RWX inline-patch reference (2026-10-10)

**Outcome: it cannot affect production behaviour, and no code change was justified.**
`hook::arch::install_inline` is *compiled* for aarch64 but is **not retained in any
artifact** and is **unreachable**; the only change made is documentation at the site
plus the evidence in this section. No feature flag was invented, nothing was deleted,
no executable-memory policy was touched, and the cdylib is byte-identical before and
after the change.

## 11.1 Source references and call-graph findings

All in `rust/uperf-sfanalysis/src/hook.rs`:

| Item | Kind | Reached from |
| --- | --- | --- |
| `install_inline` (was L345, `#[allow(dead_code)]`) | `pub fn`, **no caller** | — |
| `probe` (L319, `unsafe fn`) | helper | `install_inline` only |
| `flush_code` (L421, `unsafe fn`) | helper | `install_inline` only |
| `libc_relro_page` (L300) | helper | `install_inline` only |
| `static PROBED` (L296) | one-shot guard | `install_inline` only |
| `PROT_READ/WRITE/EXEC`, `MAP_PRIVATE/ANONYMOUS` (L289-293) | consts | `probe`/`install_inline` only |
| `mmap`, `mprotect` (`extern "C"`, L~179/L~181, aarch64-gated) | FFI decls | `probe`/`install_inline` only |
| `mod arch` (L~263) | **private** module, `#[cfg(target_arch = "aarch64")]` | — |

Searches run over `rust/` **and** `tools/` (`.rs`, `Cargo.toml`, scripts, docs):
`install_inline`, `libc_relro_page`, `flush_code`, `PROBED`, plus every `mmap`/`mprotect`
use. Hits are the definitions, their internal call sites, the doc comments, and the
"retained only as reference" note — **zero external references**. In particular:

* no **function pointer**, no array/table of them, no trait object, no vtable entry;
* no `#[no_mangle]` / `#[export_name]` anywhere in the crate except `observe`'s statics
  and `sfh_install` (lib.rs:206) — neither touches this function;
* the `global_asm!` shims declare `.global sfh_shim_*` and reference `SFH_ORIG_*` +
  `crate::observe::note`; they do not reference `install_inline`;
* not referenced by tests, examples, build scripts, `build.sh`, or the module scripts;
* the crate is a `cdylib` with no Rust consumer in the workspace, so no downstream
  crate can call it;
* the *live* hooking path is the file-scope `hook::install` → `got::find_slots` +
  `got::patch_slot` (GOT/PLT rewriting), reached from `lib.rs::install_all` ←
  `sfh_install`/`sfh_ctor`. `install` needs `arch_shim_for` (the asm shims) and
  `read_insns`/`resolve_entry`, which is why those are the only `mod arch` items that
  survive (see 11.3).

`#[allow(dead_code)]` is a **lint** attribute: it suppresses the warning, it does not
make the item unreachable-by-construction and it does not remove anything from the
build. Nothing in the call-graph conclusion rests on it.

## 11.2 Compilation and retention, per level

| Level | Verdict | Evidence |
| --- | --- | --- |
| 1. Exists in source | **yes** | its doc comment plus body is the 6 938-character block that the experiment in (3) removed, in `hook.rs` |
| 2. Compiled for aarch64 | **yes** | `mod arch` is `#[cfg(target_arch = "aarch64")]` — cfg'd *in* for the device target, and `#[allow(dead_code)]` only silences a lint. It goes through the frontend and LLVM on every aarch64 build (release **and** dev; the dev build was produced during this audit to confirm it) |
| 3. Retained in the artifact | **no** | three independent measurements, below |
| 4. Reachable / executable | **no** | §11.1: zero references from anywhere, private module, not exported |

Measurement (3), on the real `aarch64-linux-android` cdylib:

1. **Symbols.** `llvm-nm` on the **unstripped** build (`--profile release-debug`, which
   keeps the symbol table but not LTO/GC behaviour): no
   `sfanalysis_rs::hook::arch::install_inline`, no `::probe`, no `flush_code`, no
   `libc_relro_page`, in mangled or demangled form. Positive controls in the same
   dump: `sfh_install` (T), `sfh_stats` (T), `SFH_INSTALLED`, `SFH_ORIG_IOCTL`,
   `SFH_ORIG_EPOLL_WAIT` … and `sfanalysis_rs::hook::arch::arch_shim_for` (which the
   live path calls). This is also true in the **non-LTO dev build**, so the removal is
   `-Wl,--gc-sections` + per-item sections, not an LTO-specific effect.
2. **Literals.** The three `dbg_log` format strings only this path can produce —
   `mmap probe: RW ok={} …`, `mprotect probe: libc RELRO page RW ok={}` and the
   `RELRO page` fragment — are absent from the stripped release `.so`, the unstripped
   release-debug `.so`, the **dev** `.so`, and the checked-in `magisk/bin/
   libsfanalysis_rs.so`. Control: `hooked`, `NOT hooked`, `sfh_stats_loop`,
   `xh_refresh_loop`, `UPERF_SFANALYSIS_*`, `/data/misc/surfaceflinger/sfh.debug` are
   all present.
3. **Byte-level, controlled.** Deleting the whole function (6 938 chars) and rebuilding
   produced a **byte-identical** `libsfanalysis_rs.so` — same SHA-256, same size — and a
   touch-only rebuild of the unmodified source produced that same hash, which is the
   determinism control that makes the comparison meaningful. So the function
   contributes **zero bytes** to the production artifact. (Same result again after the
   documentation change in 11.5; and the determinism control was repeated at the end of
   the session — two consecutive rebuilds of the final source, both
   `dceb0c960171031999f949eea449bef0551f06da241b57a9bf305008be463829`, 300 632 B.)

Per the brief, none of this is over-read: an absent symbol proves the linker retained
no out-of-line copy, **not** that the code was never compiled (it was — level 2), and
string absence alone would not prove removal (a merged `.rodata` section can hold dead
literals). Measurement 3 closes both gaps for this configuration.

## 11.3 The RWX operations

Line numbers are as of the M12 audit; the R-2 doc comment added in §11.5 shifts the
in-function ones by +26 (symbol names are the stable reference).
| # | Site | Call | write+exec together? | Result checked? | Failure path |
| --- | --- | --- | --- | --- | --- |
| 1 | `probe(ps, R\|W)` (L367) | `mmap(null, ps, RW, MAP_PRIVATE\|MAP_ANONYMOUS, -1, 0)` | no (RW) | yes — logs `ok`/errno | returns `(false, errno)`; nothing to undo |
| 2 | `probe(ps, R\|X)` (L368) | same, RX | no (no write) | yes | as above |
| 3 | `probe(ps, R\|W\|X)` (L369) | same, **RWX**, anonymous | **yes** | yes — logged only, never acted on | as above |
| 4 | trampoline (L~392) | `mmap(null, ps, **RWX**, MAP_PRIVATE\|MAP_ANONYMOUS, -1, 0)` | **yes** | yes — `MAP_FAILED` → `HookError::Syscall("mmap", errno)` | **leak**: this mapping is never `munmap`ed if a later step fails (only reachable from the dead path) |
| 5 | entry patch (L~443) | `mprotect(entry page, **RWX**)` on a **file-backed code page** | **yes** | yes — non-zero → `HookError::Syscall("mprotect", errno)` *before* any byte is written | page left as it was (the call failed) |
| 6 | RELRO probe (L379/382) | `mprotect(libc RELRO page, RW)` then restore `R` | **no** — a *data* page, never executable | yes — logged; the restore is attempted on success | page restored to `R`; this is the *benign* case, and it is what the live GOT path in `got.rs` actually needs |

* Items 3–5 are the ones the brief means by "writable and executable permissions
  coexist"; items 1, 2 and 6 deliberately do not.
* All six are inside explicit `unsafe { … }` blocks that carry `// SAFETY:` comments
  (added in the M12 audit, §4).
* Architecture assumptions are real and contained to this path: a 16-byte
  `ldr x17,#8; br x17` + literal entry jump, an aarch64 prologue copied into the
  trampoline, and `dc cvau`/`ic ivau`/`isb` cache maintenance. `resolve_entry` and
  `is_pc_relative` *refuse* a prologue containing a PC-relative instruction rather than
  corrupting it — the failure mode is an error, not silent mis-patching.
* Documented as a historical/experimental reference: yes — the doc comment says the
  inline route is refused on this device and the code is kept as the reference for what
  an inline patch would have to do. The one-shot probe in item 3 is the *instrument*
  that established the device's policy, which is recorded in `docs/m8-sfanalysis-reverse.md`
  and in the `got.rs` header (`execmem` refused for the anonymous RWX map, `execmod` for
  mprotecting a code page).

Security implication, stated plainly: **there is no production exposure today.** The
shipped library contains neither the code nor its strings, and even the source that
does contain it cannot be called. The residual exposure is a *source*-level one — the
next person who adds a caller — and no build-level property can remove that; only
documentation can, which is what 11.5 does. If it were ever called, the paths it would
exercise are exactly items 3–5, which this device's SELinux refuses.

## 11.4 Isolation options considered

| Option | Verdict |
| --- | --- |
| 1. Retain with explicit documentation | **Selected.** The brief's condition is met and *measured*, not argued: demonstrably unreachable (§11.1) and harmless to production builds (§11.2.3 — zero bytes, byte-identical artifact). |
| 2. Move behind an existing feature gate | **Not applicable.** `uperf-sfanalysis/Cargo.toml` has no `[features]` section at all — there is no *existing* gate, and inventing one is what the brief constrains. Two reasons beyond the letter of it: a gate that no build enables would stop the reference from being **compiled** in every normal build, so it would rot silently instead of staying a living, type-checked reference — the opposite of the reason it is kept; and it would not shrink the artifact, which already contributes nothing. |
| 3. Separate non-production module / test-only target | Same effect as 2 (it is a `cfg` gate by another name), and worse here: the code cannot be exercised off-device (aarch64-only, and the two routes it needs are refused by the target's SELinux), so a test-only target would hold something that can never be run. |
| 4. Delete | Reserved by the brief for "obsolete … no continuing maintenance value". The repository's evidence is the opposite — the M8 findings about `execmem`/`execmod` are *derived* from this code, and the doc comment names it as the reference for the inline-patch layout. Out of scope for an audit, and the brief says this is "not a mandate to delete". |

No feature flag was created; nothing was deleted; no executable-memory policy, kernel
interface, public API, or architecture-specific logic was changed.

## 11.5 Exact source changes

Documentation only, two hunks, both in `rust/uperf-sfanalysis/src/hook.rs`:

1. the `install_inline` doc comment gained a `# Unreachable, and not in the shipped
   library` section: no caller / private module / not exported; `#[allow(dead_code)]`
   silences a lint but does not stop compilation, and what removes it is LTO +
   `--gc-sections`, with the three measurements summarised; plus the revival warning
   (this is the one place in the crate that asks for a writable **and** executable
   mapping, and the probe below is what established that the device refuses the RWX
   shape while the RW/RX ones succeed, and that the separate
   `mprotect`-a-code-page step is refused by `execmod` — believe a successful
   `mmap`/`mprotect` only after checking on the target);
2. the trailing "retained only as reference" comment now says it is compiled but not
   linked into the shipped library, and points at the doc comment.

No code, no `cfg`, no `Cargo.toml`, no new dependency. Verified documentation-only: the
release cdylib is byte-identical before and after
(`dceb0c960171031999f949eea449bef0551f06da241b57a9bf305008be463829`, 300 632 B).

## 11.6 Validation

| Check | Result | What it proves / does not prove |
| --- | --- | --- |
| `cargo check --workspace --all-targets` (host) | **0 errors** | the change compiles; nothing about the artifact |
| `cargo check --workspace --all-targets --target aarch64-linux-android` | **0 errors** | ditto, on the device target |
| `cargo test --workspace` (host) | **268 passed, 0 failed** | behaviour unchanged; does **not** exercise `install_inline` (nothing does) |
| `cargo clippy` host / aarch64 | **0 errors**; warnings identical to the pre-change set (`errno` dead on non-aarch64, `unreachable expression`, `statement with no effect`) | no new lint; the `dead_code` suppression is unchanged |
| `sh build.sh Release make` (C++ + NDK r30) | **exit 0**, `[100%] Built target uperf` | the full production build still links; `libsfanalysis_rs.so` is built by this script, so the artifact path is exercised |
| release cdylib SHA-256 before/after the doc edit | identical (`dceb0c96…`) | the change is documentation-only — the strongest form of "no behaviour change" |
| `llvm-nm` (unstripped release-debug `.so`) | no `install_inline`/`probe`/`flush_code`/`libc_relro_page` symbol; `sfh_install`/`sfh_stats`/`arch_shim_for` present | the linker retained no out-of-line copy. Does **not** prove it was never compiled — it was |
| `strings` on 4 artifacts (stripped release, unstripped release-debug, dev, checked-in) | 0 of the 3 path-only literals; live literals present | the constants only this path uses were dropped too. Does not by itself prove removal (a merged rodata section could keep dead literals) |
| controlled rebuild: source with vs without the function | **byte-identical** `.so` (SHA-256 + size), with a touch-only determinism control | zero contribution to the artifact **for this source, toolchain, profile and environment** |
| section dump vs the checked-in/installed artifact | `.text` (218 516 B), `.rodata`, `.eh_frame`, `.init_array`, `.data` **byte-identical**; only `.data.rel.ro` differs, in 15 bytes of 6 064 | the machine code of the current build equals the checked-in one's; the delta is relocated metadata, not code |

What this validation does **not** establish, and is therefore not claimed:

* that dead-code elimination is a *security boundary* — it is not; it is one build's
  outcome, and §11.7 records the boundary;
* that the code is correct or safe — §11.3 documents the opposite (a never-`munmap`ed
  RWX trampoline on one failure path);
* anything about non-aarch64 targets: `mod arch` does not exist there, so there is
  nothing to retain or gate;
* that any of this was re-verified on the device — **the device was not touched by this
  task** (`git status` unchanged in scope, no install, no push, no module change).

Reproducibility caveat for the two section-level rows above: every section dump was
taken from a **copy** in `/tmp`, never from the repository file. `llvm-objcopy
--dump-section .text=out <input>` with no output file rewrites `<input>` **in place** —
observed here as a 15-byte re-encoding of `.data.rel.ro` plus an mtime bump on a
tracked blob — so dumping sections from a repository artifact mutates it. The numbers
above come from a freshly rebuilt artifact and the pristine `HEAD` blob, compared as
copies; after the audit the tree carries no such mutation.

## 11.7 Remaining uncertainty

1. **The removal is a property of the build, not of the language.** It holds for the
   release profile (LTO=fat, codegen-units=1) *and* for the dev profile (no LTO) —
   both were measured — because it is `-Wl,--gc-sections` plus per-item sections that
   drop an unreferenced local. A hypothetical build configuration that disables
   section GC would retain the function as dead code (still unreachable; nothing calls
   it). That is the honest limit of the claim: "not in the artifact" is measured for
   the profiles this repository builds, not guaranteed by the type system.
2. **`install_inline`'s removal is not the same as its absence from the *sources*.**
   The RWX requests remain in the repository, and any future caller — one line —
   puts them on a production code path. Documentation is the only mitigation the
   current design permits; a feature gate would trade that risk for silent rot
   (§11.4).
3. **The checked-in `magisk/bin/libsfanalysis_rs.so` (and the copy installed on the
   device) cannot be tied to a source revision** — it was produced at 13:33, before
   this session, and its `.data.rel.ro` (line-number metadata) matches neither the
   current tree's nor conclusively `HEAD`'s rendering. It is used here for exactly one
   fact: it too contains no `install_inline` symbol or literal. It is *not* used as a
   baseline for byte-identity.
4. The 15-byte `.data.rel.ro` delta between a current build and that artifact is
   reported as an observation: **[V]** all 15 bytes lie in `.data.rel.ro`
   (0x46718–0x46890, i.e. inside 0x46690–0x47e40) and never in `.text`
   (0x10d40–0x462d4), and they sit in 24-byte records; **[I]** that is `core::panic::Location` line-number metadata, i.e. an
   artefact of source line numbers rather than of code. Decoding the records to prove
   the inference was not done.
5. `hook::arch` also contains the aarch64-only `arch_shim_for`, which *is* retained —
   it is the live path's shim lookup. R-2 does not touch it or the `global_asm!` shims,
   and neither does anything else in this change set.

## 11.8 Deliverable summary for R-2

| | |
| --- | --- |
| Can it affect production behaviour? | **No** — unreachable (0 references, private module, not exported) *and* absent from every artifact measured, in both release and dev profiles |
| Isolation change applied | **Documentation only** (options in §11.4, rationale in the report); no feature flag, no deletion, no `cfg`, no code change |
| Exact source change | `rust/uperf-sfanalysis/src/hook.rs`, 2 doc/comment hunks (§11.5); cdylib SHA-256 identical before/after |
| Host / Android checks | check 0 errors (both targets), 268 tests pass, clippy 0 errors (both targets), `build.sh Release make` exit 0 |
| Device | **not touched** (per the task) |
| Recommended follow-up (optional) | if the maintainer wants more than documentation: delete the function, or gate it **and** add a CI check that compiles with the gate on — neither is justified by the evidence gathered here |
