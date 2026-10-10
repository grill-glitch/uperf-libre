# ⑤b / AppOpt 主腿：uprobe 每线程帧计数（真机测量 → 已定形）

目标：补上 AppOpt 的 **FPS 主腿**——attach 到目标进程 `libgui.so` 的 `queueBuffer`，只数
**这个应用**的帧，条件不满足时降级到 ⑤ 已经跑通的 `--latency` 腿。

先决问题（在我上一轮标 [U] 的地方）：**这台内核对不对得起来**。已实测，全部 [V]：

## 1. 内核能力（alioth，`/proc/config.gz` + procfs）

| 项 | 值 |
|---|---|
| `CONFIG_BPF` / `CONFIG_BPF_SYSCALL` / `CONFIG_BPF_EVENTS` / `CONFIG_BPF_JIT` | **=y** |
| `CONFIG_UPROBES` / `CONFIG_UPROBE_EVENTS` | **=y** |
| `CONFIG_KPROBES` | **is not set**（所以 kprobe 路线不存在，只有 uprobe） |
| `kernel.unprivileged_bpf_disabled` | `0` |
| `net.core.bpf_jit_enable` | `1` |
| `/sys/kernel/tracing/uprobe_events` | 存在，**ksu 域可写**（实测 `write-ok`） |
| `/sys/bus/event_source/devices/uprobe/type` | **6**（uprobe PMU 存在） |
| kallsyms | `uprobe_dispatcher` / `trace_uprobe_register` 在 |

## 2. 符号（`/system/lib64/libgui.so`，1.77 MB，仅 `.dynsym`）

| 符号 | 偏移 | 说明 |
|---|---|---|
| `_ZN7android7Surface11queueBufferEONS_2spINS_13GraphicBufferEEEiPNS_24SurfaceQueueBufferOutputE` | `0xf43ec` | **主候选**：app 把一帧交给 BufferQueue |
| `_ZN7android7Surface19queueBufferInternalEP13ANativeWindowP19ANativeWindowBufferi` | `0x1138fc` | ANativeWindow 入口（同一路径的上一层） |
| `_ZN7android7Surface27hook_queueBuffer_DEPRECATEDE…` | `0x112f40` | 旧 ANativeWindow 钩子 |

`Surface::queueBuffer` 在 **RenderThread** 上被调用（trace 里逐条可见）。
text 段的 vaddr == file offset（`LOAD 0x99000/0x99000 … RE`），所以 st_value 就是给
uprobe 的**文件偏移**。

**踩坑（[V]）**：tracefs 的 `uprobe_events` **只接受 `file:offset`，不接受符号名**
（`p:name …:queueBuffer` → `sym-FAILED`）；Android 系统库被 strip，只剩 `.dynsym`，
所以**偏移必须自己从 ELF 里解**。

## 3. 路线 A：tracefs uprobe（+ perf 计数）——能触发，但计数不合用

* 注册 `p:uperf_qb /system/lib64/libgui.so:0xf43ec` → 成功，得到 tracepoint id（1334+）。
* **会触发**：trace 缓冲区里同一窗口 **990 条**命中（`RenderThread-5286 … uperf_qb:`）。
* `tracing_on` 默认 **0**，必须 save/设 1/restore（与 M8 的 atrace 同一教训）。
* **per-task 绑定数不到**：`perf_event_open(tracepoint, pid=<tgid|tid>)` → **0 hits**
  （uprobe 的 tracepoint 不认 task 绑定）。
* per-CPU 绑定能数：8 个事件合计 **220 hits / 5 s ≈ 44/s**，但那是**全局**的，会把别的
  应用的帧算进来——正是 AppOpt 明确要避免的。
* **陷阱（[V]）**：同一个 uprobe 上**同时**开 trace 消费者（`events/uprobes/x/enable=1`）
  与 perf 事件，perf 那条会**永久挂住**（`read` 不返回；`timeout` 才收场）。要数就别开
  trace 消费者。

## 4. 路线 B（**定形**）：uprobe PMU 每**线程**计数

`perf_event_open(attr{type=6(uprobe PMU), config1=<路径字符串指针>, config2=<文件偏移>,
sample_period=0}, pid=<*tid*>, cpu=-1, -1, 0)`：

| 试法 | 结果 |
|---|---|
| 偏移填 `0xdeadbeef` | `EINVAL` —— 偏移被**真正校验**，不是静默接受 |
| 绑 **主线程 tid** | **0 hits**（主线程不 queueBuffer） |
| 绑 **RenderThread tid** | **600 hits / 5 s = 120.0/s** —— 正好是 120 Hz 面板速率 |

⇒ **按线程开事件、按进程求和 = 只数这个应用的帧**，且**不需要加载任何 BPF 程序**
（eBPF 只有在想把聚合/过滤放到内核侧时才需要）。这就是主腿的计数原语。

两个必踩的实现细节（[V]）：
* `sample_period` 必须是 **0**：非 0 会被内核当成 **sampling** 事件，`read()` 去等样本、
  不返回计数（我们第一轮就是这样挂住的）。
* **必须重试 `EINTR`**：脚本里任何后台子进程退出都会用 SIGCHLD 打断 syscall，
  被当成"内核拒绝"（实测每一条都报 `Interrupted system call`）。

## 5. 实现（daemon 侧，本轮落地）

`rust/uperf-core/src/sf_uprobe.rs`：

* **`sym_file_offset(elf, symbol)`** —— 纯函数，自己解 ELF64 `.dynsym`，并把 `st_value`
  经 `PT_LOAD` 映射成**文件偏移**（Android 库多数 `vaddr == offset`，但仍按 phdr 认真映射）。
  单测用**合成 ELF** 覆盖：命中符号、有 vaddr 偏移时的映射、缺符号/非 ELF/截断不 panic。
* **`FrameCounter`** —— 按 `pid` 枚举 `/proc/<pid>/task/*`，每 tid 开一个 uprobe PMU 事件，
  `reset/enable → 窗口 → disable/求和`；`sample_period = 0`、所有 syscall 重试 `EINTR`。
* **`pids_for_package(pkg)`** —— 用 `/proc/<pid>/cmdline` 找目标进程（含 `:remote`）。
* **`UprobeTask`** —— opt-in `UPERF_SF_UPROBE=1`（`UPERF_SF_UPROBE_LIB` 覆盖库路径、
  `UPERF_SF_UPROBE_WINDOW_MS` 默认 1000）：每 tick 取 top app → 找 pid → attach →
  每窗口打 `Rust: sf-uprobe <pkg> pid=… threads=… frames=… fps=…`，并把 fps 写进共享状态。
  连接窗口为 0 **连续 3 个**就重新枚举线程（进程的 RenderThread 可能在 attach 之后才建，
  早先的 tid 事件也可能随线程退出而失效）。
* **阶梯**：`sf_binder::FrameSource` 新增 `Uprobe` 变体；进入 `Fps` 分支时若
  `sf_uprobe::healthy()` 为真，就用它的读数并**完全不碰 binder**（每次 `--latency` 都要一个
  binder 往返 + 一次 pipe 读，这是成本决定不是标签），状态文件里 `source=uprobe`。
  三级顺序：**注入 hint（M8）> uprobe（⑪）> binder `--latency`（⑤）**。

单测：`cargo test --release -p uperf-core --lib` **147 pass**（含 7 条 sf_uprobe）。

## 6. 真机 e2e（[V]，alioth `f748d277`）

`tools/uprobe-probe/e2e-shade.sh`（**锁屏下拉阴影**动画作为帧源，这样设备锁着也能测；
`UPERF_SF_UPROBE_PID` 钉住目标进程 ⇒ 这一条**只验证 ⑪ 自己的计数**，不牵扯 ④）：

```
attach      : Rust: sf-uprobe attached pinned pid=4909 events=110 of 110 threads, RenderThread=Some(5021)
独立对照     : uprobe-pmu /system/lib64/libgui.so:0xf43ec pid=5021 over 5s: 526 hits (105.2/s)
daemon 窗口  : frames=124 fps=124.0 | 91 | 91 | 122 | 95 | 97 | 115 | 105 | 92 | 104
发布状态     : source=uprobe hint_age_ms=- fps=104.0 frames=104 refresh_ns=- layer=-
```

* **符号解析**：daemon 自己从设备上的 `/system/lib64/libgui.so` 解出 **`0xf43ec`**，
  与我手工 `llvm-readelf` 的结果一致（§2）——不是硬编码。
* **计数正确**：daemon 每窗口的原始帧数（91–124）**跨在独立对照 105.2/s 上下**，
  两者同一时间窗、同一个 RenderThread。
* **阶梯生效**：状态文件里 `source=uprobe`，即 ⑤ 的任务把 ⑪ 认作 fps 源、自己不去碰
  binder（`frame source -> fps` 只在启动那一拍出现）。

踩过的两个坑（都已修）：
1. **窗口算术**：计数器每个窗口开头 `RESET`，所以窗口末读到的**就是本窗口帧数**；
   我一开始还去减上一窗口的值，结果每次都被抵消成 ~0（原始读数 114–117 与独立对照
   114.0/s 其实已经对上了，是减法把它抹掉了）。现在 `frames = 本窗口读数`。
2. **线程集合**：进程的 RenderThread 可能在 attach 之后才建，早先的 tid 事件也会随线程
   退出失效 ⇒ 连续 3 个窗口为 0 就重新枚举线程（实测会重新 attach 并恢复计数）。
   另外 attach 日志打的是**真正打开成功的事件数**（`events=110 of 110`），不是枚举数。

环境相关（[U]/注意）：设备**锁屏 + dozing** 时任何应用都不会出帧（`mWakefulness=Dozing`、
`mCurrentFocus=NotificationShade/AlternateBouncerView`），此前两次 e2e 因此读到 0——不是
probe 的问题；脚本里因此加了 `svc power stayon true` 与锁屏帧源。带 PIN 的锁屏 agent 不
尝试解锁（不猜凭据）。


