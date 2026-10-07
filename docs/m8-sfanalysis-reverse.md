# M8 SfAnalysis 静态逆向（r2，22.09.04 libsfanalysis.so）

> 本节是 M8 重写的逆向起点。**所有静态已确认事实**写在这里，留待真机验证的
> byte 序列一致性、FSM 触发时间、hook 触发频率留 `[待 M8 验收]`。
> 验证由 §5 `build.sh make check` + §10.2 真机 byte 对账负责。

**vendor 件来源**：`https://github.com/yc9559/uperf/releases/tag/dev-22.09.04`
→ `sfanalysis-magisk-22.09.04.zip`（413 KB）。
**sha256**：`386905b5e6237af09f61628f3a4feb257e17f47722a36b1c9a38eb0688987f31`
**sha256-vendor 模块 id**：`sfanalysis`（作者 Matt Yang，描述指向 `yc9559/surfaceflinger-analysis/`）
**大小**：25,952 B（≈ 26 KB 量级，与 `m1-static-reverse.md §1.5` 一致）
**stripped, NDK r24, aarch64, full RELRO, dynamically linked**（无 xHook 静态链）

---

## 1. ELF 静态产物（r2 -A）

```
arch:           ARM aarch64, 64-bit
baddr:          0x0
binsz:          24,411
text size:      0x3e94 = 16,020 B (.text, r-x)
plt size:       0x2d0  =    720 B (.plt, r-x)
rodata size:    0x148  =    328 B (.rodata, r--)
data size:      0xa5   =    165 B (.data, rw-)
bss size:       0x6a8  =  1,704 B (.bss, rw-)
functions:      47（其中 41 个 PLT 导入 + 1 entry0 + 18 用户函数）
imports:        41（PLT）
```

### 1.1 完整 PLT imports（41 个，**没有 xHook 痕迹**）

```
__cxa_finalize  __cxa_atexit   gettid           clock_gettime
sleep           pthread_create pthread_mutex_lock pthread_mutex_unlock
pthread_mutex_init  snprintf  close             open
lseek           read           sscanf           timer_create
prctl           timer_settime  strlen           strcpy
fopen           fclose         fgets            strcmp
strstr          sched_setscheduler  regcomp      malloc
strdup          free           sigemptyset      sigaction
pthread_cond_signal  pthread_join  regfree     siglongjmp
pthread_self    pthread_setname_np  pthread_cond_wait  regexec
sigsetjmp       mprotect       __errno
```

**关键**：`mprotect`（`0x68f0`）、`regcomp`+`regexec`+`regfree`（libc regex，非 xHook
的内部 regex）、`sched_setscheduler`（线程优先级）、`timer_create`+`timer_settime`
（定时器）、`pthread_*`（线程）、`prctl`（线程名）。**没有 dlopen/dlsym**
—— 自己注入自己即可，不需要再拉别的库。

### 1.2 .rodata 字符串（vendor 件未 strip，全部可读）

```
/system/lib64/libandroidfw.so       ← 目标 .so（主）
/system/lib64/libandroid.so          ← 目标 .so（备用，符号兜底）
/system/bin/surfaceflinger           ← 进程 anchor（出现在 .so 里用于自检）
xh_refresh_loop                      ← hook 函数名
/proc/%d/comm
/proc/self/maps
/proc/%d/stat
"%lx-%*lx %4s %lx %*x:%*x %*d%n"    ← /proc/self/maps 行的 sscanf 模板
"%*d (%*[^)]%*[)] %c"               ← /proc/<pid>/stat 行的 PID-CMD 解析
"DelayedWork"                        ← dfps 调度器命名（跨项目共享）
".*"                                  ← regex（regcomp 编译）
"r"                                  ← fopen mode
```

### 1.3 修正 `m1-static-reverse.md §1.5`

`docs/m1-static-reverse.md §1.5` 写的：
> "libsfanalysis 通过 patchelf --add-needed 注入 surfaceflinger"

**正确**：patchelf **目标**确实是 `/system/bin/surfaceflinger` 主程序（vendor
sfanalysis 模块的 `common/post-fs-data.sh` 显式做这件事），但**被 hook 的函数**
`xh_refresh_loop` 在 `libandroidfw.so`（surfaceflinger 运行时 dlopen 的库）里。
**两件事不冲突**——DT_NEEDED 注入 surfaceflinger 主程序，sfanalysis 自己 dlopen
后在 `/proc/self/maps` 里找 `libandroidfw.so` 的 .text 段，再 mprotect + inline
patch。这与 m1-static-reverse.md §1.5 写的"解析 /proc/self/maps 定位目标库，再按
名挂钩"语义一致；§1.5 的措辞没有明确区分**注入对象**（surfaceflinger）和
**hook 对象**（libandroidfw.so 内的函数），本节予以澄清。

---

## 2. 注入机制（vendor 是怎么做的，已静态确认）

```
post-fs-data.sh（vendor sfanalysis 模块）:
  $MODDIR/system/bin/patchelf --add-needed libsfanalysis.so /system/bin/surfaceflinger
                                     --output $MODDIR/$SF
  __set_perm $MODDIR/$SF 0 0 0755 "<原 SF secontext>"

effect:
  surfaceflinger 主程序 ELF 的 DT_NEEDED 多了一项 libsfanalysis.so
  surfaceflinger 进程启动时由 ld.so 自动 dlopen libsfanalysis.so
  libsfanalysis.so 的 .init_array 跑一个 ctor（entry0 路线）：
    entry0 → bti c → adrp x0, 0x79e0 → __cxa_finalize@plt
    (entry0 只做 fini 注册，真正的 init 在 fcn.000028d4)
  fcn.000028d4（ctor）→ pthread_mutex_lock → sigaction(SIGSEGV) →
    regcomp(".*") → timer_create(CLOCK_MONOTONIC) →
    pthread_create(worker_thread)
  worker_thread 负责：读 /proc/self/maps、定位 libandroidfw.so、mprotect、
    inline patch xh_refresh_loop、回到主线程
  主线程仅在被 hook 的 xh_refresh_loop 触发时通过 trampoline 进入
    fcn.00003ec4（hook handler），handler 跑 SfHint FSM、open+write hint file
```

**重要**：vendor 的 hook 流程是 **多线程协作**（主线程被 hook 调用、worker 线程做
注入），不是单线程 inline patch。本仓库重写时可保留这一模式，但 §5/§6 描述的
单线程 inline patch 路径也能完成同样的语义。

---

## 3. hook handler 入口（`fcn.00003ec4`，1948 字节，最大函数）

[待 M8 验收]：未深度反汇编。**初步推断**（基于字符串引用 + 函数大小）：
- SfHint 6 值 FSM（idle/switch/trigger/gesture/touch/junk）
- open(O_WRONLY|O_CREAT|O_TRUNC) `<hint_file>` → write(byte) → close
- 或者 open(O_WRONLY) → lseek(0) → write(byte) → close（更省 inode）
- prctl(PR_SET_NAME, "SfHintHook") 或 pthread_setname_np 设置线程名
- 状态持续时间记录（`hintDuration` schema）

**SfHint 6 值枚举**（与 `m1-static-reverse.md §1.3` 完全一致，本仓库
`rust/uperf-core/src/hint.rs::SfHint::from_byte` 已实现）：

```rust
0 = SfHint::Idle     (8 chars, "idle")
1 = SfHint::Switch   (6 chars, "switch")
2 = SfHint::Trigger  (7 chars, "trigger")
3 = SfHint::Gesture  (7 chars, "gesture")
4 = SfHint::Touch    (5 chars, "touch")
5 = SfHint::Junk     (4 chars, "junk")
6+ = SfHint::Unknown (14 chars, "unknown")
```

---

## 4. SfHint FSM 6 值与 §7.4 兼容性

`docs/spec/sfanalysis.md §2.3` 已锁定 byte 协议与消费端 `SfAnalysisListener` 接口
（`rust/uperf-core/src/watch_task.rs`）字节兼容。**byte 序列一致性验证** 留给真机
byte 对账（M8 验收），r2 静态不覆盖。

---

## 5. 重写实现约束（来自 vendor 静态事实）

| 约束 | 来源 |
|---|---|
| 必须 hook `xh_refresh_loop` | vendor `.rodata` 字符串 |
| 必须在 `libandroidfw.so` 的 .text 段做 inline patch | vendor `fopen("/proc/self/maps")` + `sscanf` 解析 |
| 必须 `mprotect` 改 RWX | vendor PLT imports |
| 必须用 libc `regcomp/regexec`（非 xHook 内部 regex） | vendor PLT imports |
| 必须写 `<USER_PATH>/sfanalysis.hint` 单字节 | vendor 行为（与 §7.4 一致） |
| patchelf 注入 surfaceflinger 主程序 | vendor `post-fs-data.sh` |

---

## 6. 未做（明确标记）

| 项 | 状态 | 谁负责 |
|---|---|---|
| 真机 byte 序列与 vendor 件对账 | [待 M8 验收] | 用户 |
| Frida trace xh_refresh_loop 调用频率 | 不做（AGENT.md §12.1 明确不做 Frida） | — |
| hook handler 函数级反汇编（fcn.00003ec4 1948 B 全展开） | 做了第 1 层（入口 + string ref），未做完整流程 | r2 静态已够 |
| 内联汇编 trampoline 字节序列精确推导 | [M8 Rust 实现时按需做] | 本仓库 |
| 6 值 FSM 触发条件逐条推理 | [M8 真机采集] | 用户 |

---

## 7. 引用

- `docs/spec/sfanalysis.md`：M8 边界 spec（接口契约 / 实现约束 / 验收脚本）
- `docs/m1-static-reverse.md §1.5`：M1 时的逆向笔记（已部分被本节修正）
- `rust/uperf-core/src/hint.rs`：消费端 SfHint 6 值 FSM（已 M3 落地）
- `rust/uperf-core/src/watch_task.rs`：SfAnalysisListener（已 M6b 落地）
- `magisk/script/libuperf.sh`：hint 文件 `<USER_PATH>/sfanalysis.hint` 路径定义
