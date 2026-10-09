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

## 8. SF 侧注入实测（2026-10-09，alioth/A16，SELinux enforcing）

用 `ssanalysis` 模块自带的 `injector`（ptrace + 远程 `dlopen`，见
`docs/spec/sfanalysis.md §8`）把库注入**正在运行**的 surfaceflinger —— 纯内存，
无需 patchelf、无需改磁盘。

**路径铺垫**（两个都必要，否则 `dlopen` 返回 0）：

1. `/data/local/tmp` 不行：SF 的 SELinux 域读不了 `shell_data_file`
   （实测 `avc denied { search } … name="tmp" … scontext=u:r:surfaceflinger:s0`）。
2. 改放 `/data/misc/surfaceflinger/`（`surfaceflinger_data_file`，system:system 0700，
   SF 自己的目录），但 `avc denied { **execute** } … surfaceflinger_data_file` ——
   加载 .so 需要 `execute`。`chcon u:object_r:system_file:s0` 后才放行
   （SF 本来就执行一堆 `system_file` 的 .so）。namespace 这边没问题：
   `namespace.default.permitted.paths += /data`。

**我们的库注入后**：

```
comm="xh_refresh_loop" 出现 → ctor 真的跑了、worker 线程建起来了
avc: denied { execmem } scontext=u:r:surfaceflinger:s0 tcontext=u:r:surfaceflinger:s0 tclass=process
   ×4  ← 正好是 4 个 trampoline 的 mmap(PROT_READ|WRITE|EXEC)
libc.so 仍是 r-xp，没有 rwxp
```

**vendor 库注入后**（同一路径、同一 injector）：

```
SF 里出现 fd 114 -> /system/bin/surfaceflinger
        fd 245 -> /system/lib64/libandroidfw.so
        fd 262 -> /system/lib64/libandroid.so
   ← fcn.00004d58 确实执行了
无任何 avc 记录、libc 未被 patch、没有 xh_refresh_loop 线程
```

**结论**：两条路在 enforcing 的 A16 上都**装不上 hook**，但卡点不同：

| | 卡在哪 | 结果 |
|---|---|---|
| vendor | SF 的 maps 里**没有** `libandroidfw.so`（§2 已证），它的目标库选择落空 | 静默 no-op |
| 本仓库 | 目标是 libc（处处都在），找到并走到补丁步 → SELinux `execmem` 拒绝匿名 RWX | no-op + 1 条 avc |

**vendor 模块没有发布任何 `sepolicy.rule`**（模块内容只有 `post-fs-data.sh`/
`service.sh`/`system.prop`/`patchelf`/`libsfanalysis.so`），所以它自己也没解决
`execmem`；这个特性在 enforcing 的新 Android 上本来就是死的。

**第二轮（同一天，继续深挖）**——结论是**库本身正确，唯一拦路者是 SELinux**：

用探针把三种 mmap 形态分开测（`sfh.log`）:

```
surfaceflinger 进程内,enforcing:
  mmap probe: RW ok=true err=0 | RX ok=false err=13 | RWX ok=false err=13
  NOT hooked ioctl: mmap errno=13   ×4
临时 setenforce 0:
  mmap probe: RW ok=true err=0 | RX ok=true err=0 | RWX ok=true err=0
  hooked ioctl / pthread_cond_timedwait / pthread_cond_wait / epoll_wait   ← 4/4 装上
  SF pid 不变,存活;libc 的 r-xp 段被切开(rwxp 页出现)
```

即 **`PROT_EXEC` 的匿名映射**被拒（RW 正常），且 `avc_spoof` 关掉后也**没有
avc 记录**（说明该拒绝是 `dontaudit` 或审计去重）。permissive 下 4 个 hook 全部
装上、SF 不死 → **`libsfanalysis_rs.so` 在 SF 里是可用的**。

**策略规则没能生效**：

* 模块目录放了 `sepolicy.rule`（KernelSU-Next 的字符串里确实有
  `Failed to load sepolicy.rule for`，且 `rezygisk`/`zygisk_vector` 两个模块都在用）
  → 开机后仍 EACCES。
* `ksud sepolicy patch "allow surfaceflinger surfaceflinger process execmem"` 与
  `ksud sepolicy apply <file>` 都 rc=0（`check` 也认这条语法）→ 仍 EACCES。
* 期间设备发生**两次内核 panic**（`console-ramoops-0`: `Kernel panic -
  not syncing: Fatal exception` / `Going down for restart now`），与本轮的
  `ksud sepolicy patch` 尝试时间相关（本机装有 KernelPatch/KPatch-Next，panic
  handler 由 KP 接管，call trace 为空）。
* **据此停止**在用户日常机上继续调策略，不再尝试 `ksud sepolicy patch`。

**当时的交付状态（已被 §10 取代）**：模块带上 `magisk/sepolicy.rule`，但其是否在
KernelSU-Next 上真正落地未验证。

---

## 10. 解决：GOT/PLT 改写（2026-10-09 后续，真机验证通过）

§8 停在"SELinux 挡住 execmem"上。后续三条实验把结论彻底改了。

### 10.1 三条权限探测（在 SF 内、enforcing 下）

```
mmap(PROT_READ|PROT_WRITE,  匿名)        -> ok
mmap(PROT_READ|PROT_EXEC,   匿名)        -> EACCES (13)
mmap(PROT_READ|PROT_WRITE|PROT_EXEC, 匿名)-> EACCES (13)
mprotect(file-backed 代码页, RWX)        -> EACCES (13)      ← execmod
mprotect(RELRO 数据页, RW)               -> ok              ← 关键
```

**代码页写不了，数据页能写。**

### 10.2 KernelSU-Next 侧没有可用通道（实测）

* `ksud sepolicy patch "..."` 语法被接受（rc=0，`check` 也认），**但策略不生效**。
  用受控拒绝实验确认：`/data/local/tmp/denytest`（context
  `u:object_r:keystore_data_file:s0`）对 `shell` 读被拒 → patch
  `allow shell keystore_data_file file read` → 仍被拒。
* 更关键的对照：把同一条规则写进 **`/data/adb/modules/zygisk_vector/sepolicy.rule`**
  （一个确实在用的 Zygisk 模块）并**重启**，读仍然被拒；写进 `uperf` 模块自己的
  `sepolicy.rule` 同样无效。
  → **这台设备上 KernelSU-Next 不加载任何模块的 `sepolicy.rule`**，与是否 Zygisk
  模块无关。既有模块带这个文件不等于它被消费。

### 10.3 于是改用 GOT/PLT 改写 —— 也正是 vendor 的真实做法

vendor 库**静态链的是 xHook**，而 xHook 的默认机制就是 GOT/PLT 改写，不是 inline
patch。这同时解释了为什么 vendor 件在 SF 里"没崩也没 hook"：它受制于它自己写死的
`/system/lib64/libandroidfw.so` 目标选择（A16 的 SF 不加载该库），而不是权限。

实现（`rust/uperf-sfanalysis/src/got.rs`）：

1. `dl_iterate_phdr` 遍历所有已加载对象；
2. 从 PT_DYNAMIC 取 `DT_SYMTAB/DT_STRTAB/DT_JMPREL/DT_RELA`；
3. 遍历 relocation，符号名精确匹配四个目标 → 槽地址 = `dlpi_addr + r_offset`，
   槽里现有值就是 libc 的原始地址（存下来给 shim 调用）；
4. `mprotect(槽所在页, RW)` → 写 → 还原原保护位。**只碰数据页。**

三条实现坑（都已在代码里注释）：

* **bionic 不改写 dynamic 段里的指针条目**：ET_DYN 的 `DT_STRTAB` 等仍是
  vaddr 相对值（实测 `sym=0x320 str=0x584 jmprel=0x7f0`）。必须先折叠 `dlpi_addr`，
  否则全部落入越界检查而静默跳过。
* 有一个模块交出的 `DT_STRTAB` 根本不可用 → 每处解引用前都要用模块 PT_LOAD 的
  `[lo,hi)` 做范围校验（未加校验时真机 SIGSEGV，崩在 `xh_refresh_loop`，fault addr
  `0x7f8`）。
* relocation 条目按 `DT_RELAENT` 步进，不要硬编码 24。

### 10.4 真机结果（alioth / crDroid A16 / KernelSU Next / SELinux enforcing）

```
hooked ioctl                  slots=14 orig=0x77b8b1318c
hooked pthread_cond_timedwait slots=7  orig=0x77b8b1796c
hooked pthread_cond_wait      slots=9  orig=0x77b8b178ec
hooked epoll_wait             slots=3  orig=0x77b8b25790
```

SF pid 1506 全程不变。25 秒后从 SF 内部读计数器：

```
stats ioctl=167 epoll=50 cw=28 ctw=0 txns=132 writes=126 state=1 idle_ms=282 installed=1
```

**4/4 hook 装上、活着、且在观测真实 SF 活动**（167 次 ioctl / 132 次 binder 事务 /
126 次非空写缓冲）。host 侧 `cargo test -p uperf-sfanalysis` 19 项全过。

→ 结论：**`magisk/sepolicy.rule` 不再需要，已删除**；本机 KernelSU-Next 也不加载
它。feature 在 enforcing 上不再"死于权限"。

---

## 9. 引用

- `scripts/sfanalysis-deobf.py`：§1.3/§1.4 的解密器（可复现）
- `docs/spec/sfanalysis.md`：M8 边界 spec
- `docs/m1-static-reverse.md §1.5`：M1 逆向笔记（**本节推翻其 hook 目标结论**）
- `rust/uperf-core/src/hint.rs`、`rust/uperf-core/src/watch_task.rs`：消费端（已落地）
