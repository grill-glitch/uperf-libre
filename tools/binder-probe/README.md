# binder-probe — ⑤ 直连 binder 的最小可达事务（真机验证）

`tools/binder-probe` 是一个**独立**（非 `rust/` workspace 成员）的 aarch64 探针，
用于把"daemon 内直连 SurfaceFlinger"这一步从零打通。它只做一件事：从 root 进程
open `/dev/binder` → mmap → 向 servicemanager（handle 0）发 `getService` 事务 →
读回一个 `flat_binder_object` handle。

## 构建 / 运行（alioth）

```sh
cd tools/binder-probe
cargo build --release --target aarch64-linux-android
adb push target/aarch64-linux-android/release/binder-probe /data/local/tmp/
adb shell su -c '/data/local/tmp/binder-probe /dev/binder SurfaceFlingerAIDL'
```

## 真机实测输出（2026-10-10，alioth / crDroid A16 / Enforcing）

```
--- variant A READ 256K PRIVATE ---
  mmap ok
  S0 empty: ok (read_consumed=0)
  S2 one-way + read: ok (read_consumed=8)
--- variant C RW 1M-2p PRIVATE|NORESERVE ---
  open/mmap: mmap(prot=0x3,...): Operation not permitted (os error 1)
--- getService(SurfaceFlingerAIDL) ---
  reply 32 bytes: 00000000 852a6873 00000000 01000000 00000000 00000000 00000000 0c000000
```

解析（见下）：`[u32 status=0][flat_binder_object kind=0x73682a85 (HANDLE) flags=0
binder=1 cookie=0]...` ⇒ **拿到 handle=1，事务往返成功**。

## 踩出来的四个真事实（照抄即可，别再摸一遍）

1. **读缓冲必须是可写的堆缓冲，不是 mmap。** 内核把 `BR_*` 命令流写进
   `read_buffer`；binder 的 mmap 是 `PROT_READ` 的（`PROT_WRITE` 直接 `EPERM`，
   实测），所以拿 mmap 当读缓冲必然 `EFAULT`。libbinder 用的是
   `read_buffer = mIn.data()`（Parcel 堆缓冲）。**事务 payload 仍落在 mmap 里**，
   由 `data.ptr.buffer` 指向（只读可读）。
2. **事务 data 必须带 AOSP `writeInterfaceToken` 的 vendor 头**：接收端按
   `[i32 strictPolicy][i32 workSource][i32 kHeader][string16 descriptor][args]` 读。
   `kHeader` 在 `/dev/binder` 是 `0x53595354`（"SYST"），`/dev/vndbinder` 是
   `0x564e4452`（"VNDR"）。漏了它，服务端 logs `Expecting header 0x53595354 but
   found <你的字节>. Mixing copies of libbinder?` 并丢弃事务。
3. **同步调用是两次 ioctl。** 带 `read` 的那次写回 `BR_NOOP` +
   `BR_TRANSACTION_COMPLETE` 就立刻返回（线程要回用户态）；必须再发一次**只读**的
   `BINDER_WRITE_READ` 阻塞等 `BR_REPLY`——libbinder 的 `waitForResponse` 循环。
4. **`BINDER_WRITE_READ` = `_IOWR('b',1,48)` = `0xc0306201`**（`binder_write_read`
   是 6×8=48 字节，不是 56）。`BC_TRANSACTION`=`0x40406300`、`BR_REPLY`=`0x80407203`
   与内核一致；`BINDER_VERSION` 返回协议 8。

## 复现的 ioctl 常量

| 名 | 值 |
|---|---|
| `BINDER_WRITE_READ` | `0xc0306201` |
| `BINDER_VERSION` | `0xc0046209` |
| `BC_TRANSACTION` | `0x40406300` |
| `BC_FREE_BUFFER` | `0x40086303` |
| `BR_REPLY` | `0x80407203` |
| `BR_TRANSACTION_COMPLETE` | `0x00007206` |
| `BR_NOOP` | `0x0000720c` |

## 下一步（同一队列项 ⑤）

把它扩成 SF 的 `dump` 事务：目标 handle 用本探针拿到的 `SurfaceFlingerAIDL`
handle，`code` = `dump`，data 里塞一个 pipe fd 与 `--latency <layer>` 参数，读回文本
后解析帧时间戳算 FPS。注意 SF 的 `dump()` 可能查 `android.permission.DUMP`——
本探针已证明"能建连、能往返"，权限这一层留给下一步实测。
