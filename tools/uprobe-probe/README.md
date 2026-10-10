# uprobe-probe — ⑤b（AppOpt 主腿）的计数原语

`uprobe-count` 用 `perf_event_open` 数一个 uprobe 的命中，目标是"**只数目标应用的帧**"。
两条路都实现并对过（真机结论见 `docs/m11-11-ebpf-uprobe-frame-leg.md`）：

```sh
# 路线 B（定形）：uprobe PMU，按 TID 绑定
uprobe-count pmu /system/lib64/libgui.so 0xf43ec <tid> 5
#   -> uprobe-pmu …:0xf43ec pid=<tid> over 5s: 600 hits (120.0/s)   ← RenderThread，120Hz 面板

# 一次做完 daemon 的聚合逻辑：进程的每个 tid 一个事件再求和（诊断用）
uprobe-count pmuall /system/lib64/libgui.so 0xf43ec <pid> 5
#   -> pmuall: 30 of 30 thread events opened
#   -> pmuall over 5s: 605 hits (121.0/s)
#   -> threads with hits: [(29104, 605)]            ← 全部落在 RenderThread 上

# 路线 A：tracefs uprobe 的 tracepoint id（先 `echo 'p:x /path:0xoff' > uprobe_events`）
uprobe-count <tracepoint-id> <pid|0=all> <secs> [inherit]
#   -> per-task 绑定数不到；per-CPU（pid=0）能数，但是全局的
```

## 为什么要按 **tid** 而不是 pid

调用 `Surface::queueBuffer` 的是 **RenderThread**，不是主线程：绑主线程 tid 得 0，
绑 RenderThread tid 得 600 hits/5 s。所以实现要 **枚举 `/proc/<pid>/task/*`，每个 tid
开一个事件再求和**。

## 踩过的坑（真机实测）

1. **`sample_period` 必须为 0**。非 0 → 内核当 sampling 事件 → `read()` 等样本、不返回
   计数（第一轮就是这么挂住的）。
2. **必须重试 `EINTR`**。脚本里任何后台子进程退出都发 SIGCHLD 打断 syscall，看起来像
   "内核拒绝"（每一路都报 `Interrupted system call`）。
3. **偏移要自己解**：tracefs 的 `uprobe_events` 不接受符号名（strip 过的系统库只剩
   `.dynsym`），只能 `file:offset`。
4. **别同时开 trace 消费者**：同一个 uprobe 上 `events/uprobes/x/enable=1` 与 perf 事件
   并存时，perf 那条会挂住。
5. `tracing_on` 默认 0，若走路线 A 必须 save/设 1/restore。
6. 同一个 uprobe 事件被 enable 占着时，`echo > uprobe_events` 会报
   `Device or resource busy` —— 先把 `enable` 置 0 再删。

构建：`cargo build --release --target aarch64-linux-android`（`.cargo/config.toml` 与
`tools/binder-probe/` 相同）。

## daemon 侧（⑪ 的正式实现）

`rust/uperf-core/src/sf_uprobe.rs`（opt-in `UPERF_SF_UPROBE=1`）就用了上面的 PMU 原语：
自己解 `.dynsym` 拿偏移 → 枚举 `/proc/<pid>/task/*` → 每 tid 一个事件 → 每窗口求和 = 该应用
的 fps。`sf_binder` 的阶梯里它排在注入 hint 之后、binder `--latency` 之前，健康时状态文件
写 `source=uprobe`。

两个 e2e：

* `e2e-shade.sh` —— 用**锁屏下拉阴影**当帧源（设备锁着也能跑），`UPERF_SF_UPROBE_PID` 钉住
  目标进程，同时用本工具对同一个 RenderThread 做独立对照。实测 daemon 窗口 fps 91–124 vs
  独立对照 105.2/s。
* `e2e-leg.sh` —— 产品路径：`UPERF_SF_BINDER=1` + `UPERF_SF_UPROBE=1`，帧源是新鲜启动的
  Settings 列表（**只往上滑**；交替下滑会拉出 Settings 的搜索页，那页没得滚）。
  注意设备**锁屏/dozing 时任何应用都不出帧**，脚本里会 `svc power stayon true`。
