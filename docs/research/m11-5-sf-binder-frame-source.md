# ⑤ 直连 binder 帧源 —— 研究 + 真机事实（**实现未开始**）

队列项 ⑤："直连 binder 帧源（AppOpt 降级腿；已实测 ksu→SF binder 可达、spawn dumpsys
每次 ~44 ms 不可用，真形态是 daemon 内直连 binder）"。

标注：[V] 真机实测 / [I] 推断 / [U] 未知。

---

## 1. 设计已定位：AppOpt = `cinitdev/AppOpt`

`app_process` + `TaskStackListener` 前台助手（④）与"降级腿"帧源（⑤）**同源**于此仓库。

参考快照：`git clone https://github.com/cinitdev/AppOpt.git` @ `5176dcc56bfdbedc09e2c4e7cbc84a0c163d3067`
（2026-10-08），本地只读副本在 `~/.hermes/cache/scratch/AppOpt`（缓存目录会被清理，需要时重克隆）。

AppOpt 的 FPS 采集优先级（README）：eBPF（Rust/aya uprobe 到目标 PID `libgui.so`
的 `queueBuffer`）→ **`SurfaceFlinger --latency`** → `--timestats`；FPS 数据经 App
创建的本地 socket 推送，socket 不可用再写 App 私有目录 fps 文件兜底。

关键实现（`native_daemon/daemon_rs/src/fps_core/`）：

* `binder.rs` —— **就是"直连 binder 帧源"**：注释明确"没有使用 dumpsys 命令，也没有
  依赖 Android Java API：向 servicemanager 查询 `SurfaceFlinger` handle；对
  SurfaceFlinger 发 **dump transaction**，并通过 pipe 取回文本"。
* `fallback.rs` —— 优先 binder 直连 dump：先 `--latency <layer>`（由图层帧时间戳算
  FPS），连续失败切 `--timestats`；**binder 不可用时**才最终降级到"有硬超时的低频
  dumpsys timestats"。

即：本项目的 ⑤ = 把"spawn dumpsys"换成"进程内直连 binder"，与 AppOpt 的 mid-tier
降级腿同构。

## 2. 许可红线（**必须遵守**）

* AppOpt 仓库**没有顶层 `LICENSE` 文件**；`native_daemon/fps_monitor/qixia_ebpf_bridge/Cargo.toml`
  声明 `license = "GPL-3.0"`；`daemon_rs` 未声明许可。
* 本仓库是 **Apache-2.0**。**不得拷贝 AppOpt 任何代码**（GPL-3.0 与本仓许可不兼容）。
  只能作为**设计参考**；实现须自研——与 ④ 一样的处理（④ 的 Java 助手是本仓独立写的）。

## 3. 真机事实（alioth / crDroid A16 / Enforcing，本轮实测）

* [V] SF 发布了**两个** binder 服务：
  * `SurfaceFlinger` → `android.ui.ISurfaceComposer`（legacy）
  * `SurfaceFlingerAIDL` → `android.gui.ISurfaceComposer`（AIDL）
* [V] **legacy 路径对 root 拒绝**：`service call SurfaceFlinger 1` →
  `Result: Parcel(Error: 0xffffffffffffffff "Operation not permitted")`。与
  `docs/research/sf-backdoor-probe-verdict.md` 的结论一致。
* [V] **AIDL 路径对 root 可达**：`service call SurfaceFlingerAIDL 1` → 正常 Parcel，
  无拒因。⇒ 队列里"ksu→SF binder 可达"成立，但走的是 **AIDL** 服务，不是 legacy
  的 `android.ui.*`（旧 doc 只否掉了 legacy，没有否掉 AIDL）。
* [V] `dumpsys SurfaceFlinger --latency` 以 root 可用；本次实测 fork+exec 全程
  **~0.02 s**（队列记的 ~44 ms 应是更重的调用或更早的测量）。`/dev/binder`（→
  `/dev/binderfs/binder`）权限 0666，root 可 open。
* [U] 仍未知（要先用自研 binder 客户端才能测）：SF 的 `dump()` 事务对 root 的权限
  （是否需要 `android.permission.DUMP`）、目标 layer 名从哪来、`--latency` 文本表的
  精确格式与采样窗口。

## 4. 第一轮：从零 binder 客户端，真机跑通（2026-10-10）

`tools/binder-probe/`（独立 crate，非 `rust/` 成员）已把最小可达事务打通并**真机
验证**：root → open `/dev/binder` → `mmap(PROT_READ)` → `BINDER_VERSION`=8 →
向 servicemanager(handle 0) 发 `getService("SurfaceFlingerAIDL")` → 收到 32 字节回复，
解出 `flat_binder_object`：`kind=0x73682a85`（BINDER_TYPE_HANDLE）`binder=1`。
⇒ **"daemon 内直连 binder"这条路在真机上成立**。

踩出来的四个真事实（写进 `tools/binder-probe/README.md`，别再摸一遍）：

1. **读缓冲必须是可写的堆缓冲，不是 mmap。** 内核把 `BR_*` 命令流写进
   `read_buffer`；binder 的 mmap 是 `PROT_READ`（`PROT_WRITE` 实测 `EPERM`），用
   mmap 当读缓冲必 `EFAULT`。payload 仍落在 mmap，由 `data.ptr.buffer` 只读读。
2. **事务 data 必须带 AOSP `writeInterfaceToken` 的 vendor 头**：
   `[i32 strictPolicy][i32 workSource][i32 kHeader][string16 descriptor][args]`，
   `kHeader` = `0x53595354`（/dev/binder 的 "SYST"；vndbinder 是 `0x564e4452`）。
   漏了它服务端打 `Expecting header 0x53595354...` 并丢弃事务。
3. **同步调用是两次 ioctl**：写回 `BR_TRANSACTION_COMPLETE` 即立刻返回，须再发一次
   只读 `BINDER_WRITE_READ` 阻塞等 `BR_REPLY`（libbinder 的 `waitForResponse`）。
4. `BINDER_WRITE_READ` = `0xc0306201`（`binder_write_read` 是 48 字节）。常量表见
   README。

## 5. 第二轮：SF `dump` 腿真机跑通（2026-10-10）

在 `tools/binder-probe/` 上把"经 binder 取 SF 帧源"打通并真机验证：

```
$ binder-probe /dev/binder --latency
8333333                        # = dumpsys SurfaceFlinger --latency（120 Hz 周期 ns）
$ binder-probe /dev/binder --latency 'com.android.launcher3/…QuickstepLauncher#260'
8333333
11623250883013	11623273240930	11623257433117
11623260714888	11623281479576	11623261508586   # 与 dumpsys 同 layer 的表逐行一致
```

即 **帧源可以在 daemon 内直取**（省掉每次 fork+exec），这是 ⑤ 的传输层交付。

关键配方（细节与 9 条坑见 `tools/binder-probe/README.md`）：

* 目标 = **legacy** `SurfaceFlinger`（不是 `SurfaceFlingerAIDL`：AIDL 的 `onTransact`
  不回落 `BBinder::onTransact`，`dump` 无响应）；
* `code = DUMP_TRANSACTION = 0x5f444d50`（base `IBinder`，与方法码无关）；
* parcel = `[fd 对象][String16[] args]`，**fd 最前、无 interface token**；
* `fd` 用 `BINDER_TYPE_FD = 0x66642a85`，并带 offsets 数组；读回用**并发线程**；
* `dump` **不回包**，故只写发送 + pipe 静默超时结束。

## 6. 未做（⑤ 的剩余）

`--latency` 文本表解析（首行 = 刷新周期；每行 `desiredPresent / actualPresent /
frameReady`）→ 滑窗 FPS；再与既有帧源（M8 注入写的 `sfanalysis.hint`）接优先级与降级，
并把这段 binder 客户端从探针搬进 daemon（`rust/uperf-core/src/`）。仍不臆造。
