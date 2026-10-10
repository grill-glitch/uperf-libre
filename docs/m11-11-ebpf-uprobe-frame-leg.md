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

## 5. 结论与下一步

[V] 主腿**能做成**，形状是：
1. 目标 = 前台应用（④ 的 top app）→ `/proc/<pid>/task/*` 枚举**每个 tid**；
2. 每个 tid 对一个候选符号开一个 uprobe PMU 事件（`Surface::queueBuffer`，回退
   `queueBufferInternal`）；
3. 每窗口 `RESET/ENABLE → 睡 → DISABLE → read` 求和 → FPS；
4. 失败（没有 PMU/库/符号、perf 被拒）→ 降级到 ⑤ 的 `--latency` 腿；
   `--timestats` 仍未做。

[TODO] 还要写：从 ELF `.dynsym` 解偏移（tracefs 不接受符号名，见 §2）、按 tid 枚举、
把这条腿接进 `sf_binder` 的 `choose_source` 阶梯、以及 daemon 侧的 tick 与日志。

工具与实测脚本：`tools/uprobe-probe/`（`uprobe-count pmu <path> <off> <pid|tid> <secs>`）。
