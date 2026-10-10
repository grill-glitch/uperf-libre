# binder-probe — ⑤ 直连 binder 帧源（真机验证）

从零写一个 root binder 客户端，**不 spawn `dumpsys`、不依赖 Java/NDK binder**：open
`/dev/binder` → mmap → servicemanager 取 `SurfaceFlinger` handle → 对 SF 发 `dump`
事务，参数经 **pipe fd** 递进去，SF 把 dump 文本写进 pipe，另一个线程读回。这就是
AppOpt `--latency` 降级腿的同构形态，只是去掉了 fork+exec。

## 构建 / 运行（alioth）

```sh
cd tools/binder-probe
cargo build --release --target aarch64-linux-android
adb push target/aarch64-linux-android/release/binder-probe /data/local/tmp/
adb shell su -c '/data/local/tmp/binder-probe /dev/binder --latency'
```

## 真机实测（2026-10-10，alioth / crDroid A16 / Enforcing）

```
$ binder-probe /dev/binder --latency
8333333

$ dumpsys SurfaceFlinger --latency
8333333

$ binder-probe /dev/binder --latency 'com.android.launcher3/…QuickstepLauncher#260'
8333333
11623250883013	11623273240930	11623257433117
11623260714888	11623281479576	11623261508586
…                       ← 与 dumpsys SurfaceFlinger --latency <同一 layer> 逐行一致
```

即：**帧源（`--latency` 的每帧时间戳表）已能从 daemon 侧经 binder 直取**，与 dumpsys
同源同值，且省掉每次 fork+exec。

## 踩出来的坑（都真机实测过，别再摸一遍）

| # | 事实 |
|---|---|
| 1 | **读缓冲必须是可写的堆缓冲，不是 mmap**。内核把 `BR_*` 命令流写进 `read_buffer`；binder 的 mmap 是 `PROT_READ`（`PROT_WRITE` 实测 `EPERM`），拿它当读缓冲必 `EFAULT`。payload 仍落在 mmap，由 `data.ptr.buffer` 只读读（libbinder 也是 `read_buffer=mIn.data()`）。 |
| 2 | **事务 data 必须带 AOSP `writeInterfaceToken` 的 vendor 头**：`[i32 strictPolicy][i32 workSource][i32 kHeader][string16 descriptor][args]`，`kHeader=0x53595354`（/dev/binder 的 "SYST"；vndbinder 是 `0x564e4452`）。漏了服务端丢弃并 log `Expecting header …`。 |
| 3 | **同步调用是两次 ioctl**：写回 `BR_TRANSACTION_COMPLETE` 即返回，须再发只读 `BINDER_WRITE_READ` 阻塞等 `BR_REPLY`。 |
| 4 | **回包里的 handle 必须 `BC_ACQUIRE`**，否则下一次用它就是 `got transaction to invalid handle`（内核只在 buffer 未释放前替你持引用）。 |
| 5 | **`BINDER_TYPE_FD = B_PACK_CHARS('f','d','*',B_TYPE_LARGE) = 0x66642a85`**。写错类型时内核 `binder_validate_object` 返回 0，报 `invalid offset (…, min …, max …) or object` + `BR_FAILED_REPLY`；`BINDER_TYPE_HANDLE=0x73682a85`。 |
| 6 | **`dump` 的 fd 必须在 parcel 最前、且不能带 interface token**。带 token 时服务端 `readFileDescriptor()` 把 token 首字节当对象读 → fd 无效 → 回包里是异常串（实测 AMS：`status=-2` + `Allocation…`，pipe 0 字节）。 |
| 7 | **要对 legacy `SurfaceFlinger` 发 `dump`，不要对 `SurfaceFlingerAIDL`**。AIDL 那个的 `onTransact` 不回落 `BBinder::onTransact`，`dump` 事务石沉大海（空回包、pipe 0 字节）。`service call SurfaceFlinger 1` 对 root 被拒是另一回事（只挡普通方法码，不挡 `dump`）。 |
| 8 | **`dump` 不回包**：SF 写完 pipe 就结束，不回 `BR_REPLY`。若按同步调用等回包会永久阻塞（strace 实证：reader 已收到文本、caller 仍卡在第二次 ioctl）。故 `dump` 走**只写发送** + pipe 的"静默 1 s 即结束"读法。 |
| 9 | 读 pipe 必须**并发**：一个 >64 KiB 的 dump 会把 64 KiB 的 pipe 缓冲写满，若等 call 返回后才读就死锁（对照实测：`dumpsys activity` 回包 4 字节、pipe 548 KB）。 |

## 常量表

| 名 | 值 |
|---|---|
| `BINDER_WRITE_READ` | `0xc0306201`（`binder_write_read` 48 字节） |
| `BINDER_VERSION` | `0xc0046209` |
| `BC_TRANSACTION` / `BC_FREE_BUFFER` / `BC_ACQUIRE` | `0x40406300` / `0x40086303` / `0x40046305` |
| `BR_REPLY` / `BR_TRANSACTION_COMPLETE` / `BR_NOOP` / `BR_FAILED_REPLY` | `0x80407203` / `0x00007206` / `0x0000720c` / `0x00007211` |
| `BINDER_TYPE_HANDLE` / `BINDER_TYPE_FD` | `0x73682a85` / `0x66642a85` |
| `DUMP_TRANSACTION` | `0x5f444d50`（`B_PACK_CHARS('_','D','M','P')`） |
| `kHeader`（/dev/binder） | `0x53595354`（"SYST"） |

## 下一步（⑤ 剩余）

`--latency` 表解析（首行 = 刷新周期 ns；其后每行 `desiredPresent / actualPresent /
frameReady`）→ 滑窗 FPS；再与既有帧源（M8 注入写的 `sfanalysis.hint`）接优先级/降级，
并落进 daemon（`rust/uperf-core/src/`）而不是探针。
