# M8 SfAnalysis 静态逆向（r2，22.09.04 libsfanalysis.so）

> M8 重写的逆向起点。**静态已确认事实**写在这里；真机 byte 一致性留在 §7。

**vendor 件来源**：`https://github.com/yc9559/uperf/releases/tag/dev-22.09.04`
→ `sfanalysis-magisk-22.09.04.zip`。
**sha256**：`386905b5e6237af09f61628f3a4feb257e17f47722a36b1c9a38eb0688987f31`
**大小**：25,952 B（aarch64、stripped、NDK r24、full RELRO、静态链 xHook）

---

## 0. 结论速览（**推翻了 m1-static-reverse.md §1.5 的 hook 目标**）

| 项 | m1 的结论（错） | 本节结论（r2 实证） |
|---|---|---|
| hook 目标 | `xh_refresh_loop`（以为是 libandroidfw 内的函数） | **`ioctl` / `epoll_wait` / `pthread_cond_timedwait` / `pthread_cond_wait`**（libc 函数） |
| `xh_refresh_loop` 是什么 | “被 hook 的函数名” | **xHook 自己的后台线程名**（`pthread_setname_np(self, "xh_refresh_loop")`）——**不是 hook 目标** |
| 那 4 条不透明 `.data` 记录 | 在 uperf 主二进制里、静态解不出 | **在 libsfanalysis.so 里**，是 **TEA 变体加密的 4 个符号名**，已解密（见 §1.5 / `scripts/sfanalysis-deobf.py`） |
| `libandroidfw.so` / `libandroid.so` 字符串 | 被 hook 的函数所在库 | **hook 目标库的选择集合**（maps 扫描时匹配），不是被 hook 符号的所在 |

**为什么重要**：重写必须 hook 那 4 个 libc 函数，而不是 `xh_refresh_loop`。
`xh_refresh_loop` 在任何 libandroidfw 里都不存在，所以照 m1 结论写出来的 hook
永远匹配不上（本仓库 `rust/uperf-sfanalysis` 的 `HOOK_TARGET_SYM` 就是错的）。

---

## 1. ELF 静态产物（r2 -A）

```
arch:           ARM aarch64, 64-bit
binsz:          24,411
text size:      0x3e94 = 16,020 B (.text, r-x)
plt size:       0x2d0  =    720 B (.plt, r-x)
rodata size:    0x148  =    328 B (.rodata, r--)
data size:      0xa5   =    165 B (.data, rw-)
bss size:       0x6a8  =  1,704 B (.bss, rw-)
functions:      47（41 PLT 导入 + entry0 + 18 用户函数）
```

### 1.1 PLT imports（41）

```
__cxa_finalize __cxa_atexit gettid clock_gettime sleep
pthread_create pthread_mutex_lock pthread_mutex_unlock pthread_mutex_init
snprintf close open lseek read sscanf timer_create prctl timer_settime
strlen strcpy fopen fclose fgets strcmp strstr sched_setscheduler
regcomp malloc strdup free sigemptyset sigaction pthread_cond_signal
pthread_join regfree siglongjmp pthread_self pthread_setname_np
pthread_cond_wait regexec sigsetjmp mprotect __errno
```

关键：`mprotect`（inline 改写需要）、`regcomp/regexec/regfree`（**libc regex**，
就是那条 `.*`）、`sched_setscheduler`（hook 线程设 FIFO）、`timer_create`、
`pthread_cond_wait`（**它自己链接了 pthread_cond_wait** → 它能 hook 同名的自己）。
**没有 dlopen/dlsym** —— 目标符号靠 `/proc/self/maps` + ELF 解析自定位。

### 1.2 .rodata 字符串（全部可读）

```
/system/lib64/libandroidfw.so     ← hook 目标库集合（主）
/system/lib64/libandroid.so       ← hook 目标库集合（备）
/system/bin/surfaceflinger        ← 进程 anchor
xh_refresh_loop                   ← xHook 线程名（**不是符号名**）
/proc/%d/comm, /proc/self/maps, /proc/%d/stat
"%lx-%*lx %4s %lx %*x:%*x %*d%n"  ← /proc/self/maps 行 sscanf 模板
"%*d (%*[^)]%*[)] %c"             ← /proc/<pid>/stat PID-CMD 解析
"DelayedWork"                     ← 延迟任务名（与 dfps 同源）
".*"                              ← regcomp 编译的 regex
"r"                               ← fopen mode
```

### 1.3 四条加密记录（`.data` @ vaddr 0x8d60，共 165 B 段的一部分）

```
0x8d60  3272 379e 9724 827d 2033 a8d2 b5        count=1
0x8d70  3172 379e 597b 165a 2f26 821e f8ce 4ffb db11 0ffd b5   count=2
0x8d88  3072 379e a900 40f2 44e9 6513 b80a 7e1e 97ae 81a6 5d04 ac3e b16a 3461 b5  count=3
0x8da8  3072 379e a900 40f2 44e9 6513 32c1 869a 6dbd c154 b278 33ed d11e 511c b5  count=3
```

布局：`[u32 count ^ 0x9e377233][count×8 字节密文][0x00]`（写时把末尾 `b5` 覆盖成 0）。
cipher 常量：delta `0x61c88647`（= `-0x9e3779b9 mod 2^32`），
key `[0xe9, 0x91d, 0x5b25, 0x38f75]`，状态初值 `0x28b7bd67` / `0xc6ef3720`（= `delta×32`），
32 轮。完整解析器：`scripts/sfanalysis-deobf.py`（独立跑通）。

### 1.4 解密结果（**M8 最关键的一条**）

```
ioctl
epoll_wait
pthread_cond_timedwait
pthread_cond_wait
```

语义：`ioctl` = surfaceflinger 往显示驱动推帧（**正在渲染**）；
`epoll_wait` / `pthread_cond_wait` / `pthread_cond_timedwait` =
surfaceflinger 阻塞等待（**空闲**）。hint 就是从这些调用的时序推出来的。

### 1.5 相关函数映射（r2 静态）

| 函数 | 作用 |
|---|---|
| `entry0` (0x287c) | 只做 fini 注册 |
| `fcn.000028d4` (0x29c4) | ctor：`pthread_mutex_lock` → `sigaction(SIGSEGV)` → `pthread_create(worker)` |
| `fcn.0000318c` | worker：`pthread_setname_np(self,"xh_refresh_loop")` 后循环 `cond_wait` → 调 `fcn.00003298` |
| `fcn.00003298` | 读 `/proc/self/maps`、按 `%lx-%*lx %4s …` 解析、用 `regexec` 匹配目标库 |
| `fcn.00003ec4` (1948 B) | **djb2 哈希表查找**（`h = h*33 + c`）+ `strcmp` 命中 → 符号表查询 |
| `fcn.00004d58` | 分配 hook 槽（0x18 字节表、上限 32）+ `sched_setscheduler(FIFO)` + `timer_create` |
| `fcn.00005090` | **密文解密器**（`sleep(60)` 后跑，见 §1.3） |
| `fcn.00005c88` | 引用 `fcn.00003ec4` 的调用方 |

---

## 2. 注入机制（静态确认）

```
vendor sfanalysis 模块 common/post-fs-data.sh:
  patchelf --add-needed libsfanalysis.so /system/bin/surfaceflinger --output $MODDIR/$SF

surfaceflinger ELF 的 DT_NEEDED 多一项 → 启动时 ld.so 自动 dlopen libsfanalysis.so
  .init_array → fcn.000028d4（ctor）
    → sigaction(SIGSEGV)、起 worker 线程
  worker（"xh_refresh_loop"）：周期 sleep → 读 /proc/self/maps → 找目标库 →
    mprotect 目标库 .text 为 RWX → 在目标库里给 4 个 libc 符号名装 hook
  hook 触发时进入 handler → 推 SfHint 状态 → 写 <USER_PATH>/sfanalysis.hint 单字节
  （解密器 fcn.00005090 延迟 60s 跑，解密 §1.4 的 4 个名字）
```

**注意（真机实测，2026-10-09，alioth/A16）**：
`surfaceflinger` 的 `/proc/<pid>/maps`（431 个 .so）与 DT_NEEDED **都不含
`libandroidfw.so`**；该库只出现在 zygote / system_server / 各 App 进程里。
所以 §1.2 的 `libandroidfw.so` 目标库集合在 A16 的 SF 里匹配不到 ——
**vendor 件在这台设备上也会静默失效**（不是本重写引入的回归）。
这与 §1.4 的 libc 目标并不矛盾：libc 在每个进程都有，但 maps 扫描的库选择
若只认 libandroidfw，那在 A16-SF 上就没有可 hook 的挂点。

---

## 5. hook 注册与 handler（r2 深挖，2026-10-09）

解密器 `fcn.00005090` 解出 4 个名字后，经 `fcn.000028d0`（注册助手）逐个登记：

| 符号 | 替换函数 | old_func 槽 |
|---|---|---|
| `ioctl` | `fcn.00005464` | `0x8ff0` |
| `pthread_cond_timedwait` | `fcn.000056a0` | `0x8ff8` |
| `pthread_cond_wait` | `fcn.000056d0` | `0x9000` |
| `epoll_wait` | `fcn.00005700` | `0x9008` |

替换函数形状（**先调原函数、存返回值，再跑状态机、返回原值**）：

* `fcn.000056a0` / `fcn.000056d0`（两个 cond_wait）：
  `ldr x8,[slot]; blr x8; mov w19,w0; bl fcn.00004a28; mov w0,w19; ret`
  —— 纯观察者，状态机在 `fcn.00004a28`。
* `fcn.00005464`（ioctl）：先调原函数，然后匹配 `x1 == 0xc0306201`
  —— 即 **`BINDER_WRITE_READ`**（`_IOWR('b',1,struct binder_write_read)`，
  size 0x30=48 字节）。命中后校验参数结构、加锁、调 `fcn.00005c88`。
  **所以它在解析 binder 事务**（探测 activity/焦点变化）。
* `fcn.00005700`（epoll_wait）：先调原函数；再 `gettid()` 与 ctor 记录的 tid 比对，
  命中才 `clock_gettime` + 状态机（`fcn.000058d8`），带 800ms 阈值。

状态机家族（`fcn.00006000` / `0x60e0` / `0x6184` / `0x6210` / `0x6294`）形状相同：

```
if state == 2:
    fd = *(i32*)(0x8fcc + 0xcc)      /* = 0x9098，见下 */
    if fd >= 1:
        lseek(fd, 0, SEEK_SET); read(fd, buf, 1);   /* 读 1 字节 */
```

`fcn.00004d58` 用 `open(path, O_RDONLY|O_NONBLOCK|O_CLOEXEC)` 打开三个文件并记 fd：
`/system/bin/surfaceflinger`、`/system/lib64/libandroidfw.so`、`/system/lib64/libandroid.so`
（fd 存 `.bss`：`0x9094` = libandroidfw，**`0x9098` = libandroid**）。
状态机读的那 1 字节来自 **`libandroid.so` 的第 0 字节**（ELF magic `0x7f`），
不是任何 hint 通道。

`fcn.0000658c` = 排一个 DelayedWork（回调 + 到期时间），
`fcn.0000637c` = 定时器回调（`prctl(PR_SET_NAME,"DelayedWork")` + 遍历到期任务、
执行回调）。这就是 `.rodata` 里 `DelayedWork` 串的用途。

---

## 5b. 进程内可见副作用（strace 实测）

| 时机 | syscall |
|---|---|
| 加载 | `sched_setscheduler(0, SCHED_FIFO, prio 3)`（把调用线程设成实时） |
| 加载 | `timer_create(CLOCK_MONOTONIC, SIGEV_THREAD_ID, SIGRTMIN, tid=新线程)` |
| 加载 | `open("/system/bin/surfaceflinger"…)`、`open(libandroidfw.so)`、`open(libandroid.so)` |
| worker | `nanosleep(60)` → 反复 `open/read/close("/proc/self/maps")` → 16 组 `mprotect(RW)`/`mprotect(R)` |
| 全程 | **零文件写**（无 `write`/`pwrite`/`syscall`/`mmap` 导入） |

**注**：`libandroidfw.so` / `libandroid.so` 是它**打开来解析 ELF 找符号**用的
（用来定位那 4 个 libc 符号？）——见 §5 的 open 与 `0x9094/0x9098`。
A16 上 SF 不 map libandroidfw，但这两个文件在盘上存在，所以 `open` 仍成功。

---

## 3. SfHint 6 值枚举（与消费端一致）

```rust
0 = Idle("idle")   1 = Switch("switch")  2 = Trigger("trigger")
3 = Gesture("gesture")  4 = Touch("touch")  5 = Junk("junk")  6+ = Unknown("unknown")
```

本仓库 `rust/uperf-core/src/hint.rs::SfHint::from_byte` 已实现（M3 落地）。

---

## 4. 对重写的约束（**已按本节结论更新**）

| 约束 | 来源 |
|---|---|
| hook 目标 = `ioctl` / `epoll_wait` / `pthread_cond_timedwait` / `pthread_cond_wait` | §1.4 解密结果 |
| 从调用时序推 SfHint（ioctl=渲染，等待=空闲） | §1.4 语义 |
| 需要 `mprotect` 改目标库 .text | vendor imports |
| 目标库选择走 `/proc/self/maps` + regex（集合含 libandroidfw/libandroid） | §1.2 / §1.5 |
| 写 `<USER_PATH>/sfanalysis.hint` 单字节 | §7.4 |

---

## 5. 未做 / 不确定（诚实标记）

| 项 | 状态 |
|---|---|
| 4 个 hook 的**装法**（inline patch vs GOT/PLT）细节 | [I]：走 mprotect + 改写，具体指令序列未逐条反推 |
| hint 从 ioctl/等待时序到 0..5 的**确切判定式** | [U]：`fcn.00003ec4` 之后的 handler 未完整反推 |
| `fcn.00003298` maps 匹配里 regex 的确切模式 | [I]：`.rodata` 只有 `.*`，可能在运行时另建 |
| 真机 byte 对账 | [U] 留给 M8 验收 |
| 内联汇编 trampoline 字节 | [U] 实现时按需 |

---

## 6. 生产端不存在（真机证据，2026-10-09）

`AGENT.md §7.4` / §12.2 假设 `sfanalysis.hint` 由 vendor `libsfanalysis.so` 写入。
**真机证据表明没有生产者。** 三证独立：

1. **符号表**：`libsfanalysis.so` 的全部 43 个未定义符号里**没有**
   `write` / `pwrite` / `syscall` / `mmap` / `pipe` / `eventfd` / `sendto`
   —— 它没有任何写文件或建 IPC 通道的能力（只有 `open/lseek/read/fopen/fgets`，全读）。
   `libssanalysis.so` 同理。
2. **strace 全量追踪**：NDK harness 同时 `dlopen(libandroidfw.so)` +
   `dlopen(libsfanalysis_vendor.so)`，等过它的 `sleep(60)` 解密延迟，再调用四个被 hook
   的函数，`strace -f` 抓 **1733 行** syscall：写标志 open = **0**，`write` = 只有
   harness 自己的 stdout/stderr。worker 线程（独立 pid）确实跑了：`nanosleep(60)` →
   反复读 `/proc/self/maps` → 16 组 `mprotect(RW)`/`mprotect(R)` 补代码，
   全程无任何文件写入。
3. **strace 运行中的 daemon**：`pidof uperf` 两个进程全 trace 15 s（29129 行），
   唯一命中 hint 的行为是 worker **反复**
   `openat("/sdcard/Android/yc/uperf/sfanalysis.hint", O_RDONLY|O_CLOEXEC) = -1 ENOENT`
   —— 消费端在轮询一个**永远不存在**的文件；窗口内写标志 open = 0。

**结论**：`libsfanalysis.so` 是一个**纯进程内代码补丁库**（hook 4 个 libc 函数、
补 16 个 hook 点），不产出 hint 文件；daemon 的 `SfAnalysisListener` 在读一个
没有生产者的文件。**"重写 hint 生产者" 这个前提不成立**——上游这个特性是
未完成/已废弃的（配置键与监听器都在，生产者缺失）。

对 M8 的处置见 `docs/spec/sfanalysis.md §8` 与 `AGENT.md §12.1` 的更新。

---

## 7. 引用

- `scripts/sfanalysis-deobf.py`：§1.3/§1.4 的解密器（可复现）
- `docs/spec/sfanalysis.md`：M8 边界 spec
- `docs/m1-static-reverse.md §1.5`：M1 逆向笔记（**本节推翻其 hook 目标结论**）
- `rust/uperf-core/src/hint.rs`、`rust/uperf-core/src/watch_task.rs`：消费端（已落地）
