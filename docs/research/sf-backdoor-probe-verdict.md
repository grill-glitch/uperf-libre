# sf 后门在 alioth 实测：拒因 + 完整 transcript

**结论：上游 dfps 的 `SyncCallSurfaceflingerBackdoor`（`service call SurfaceFlinger 1035/1036`）在 alioth 上对 shell UID（uid=2000）以及 root UID（uid=0）都不可用。**

## 拒因（关键证据）

按子代理的最终 verdict：

> The rejection is **not** a SELinux/UID issue — the same calls also fail as root
> via `su -c 'service call SurfaceFlinger 1 i32 0'` (uid 0, ksu context), and even
> benign transaction code `1` (BOOT_FINISHED) returns `Operation not permitted`.
> SurfaceFlinger\'s own per-transaction permission check refuses every caller that
> comes through the legacy `android.ui.ISurfaceComposer` binder path reachable
> via the `service call` helper, so the `SyncCallSurfaceflingerBackdoor` notify-file
> trigger path is unreachable.

## logcat 节选（探针窗口内 SurfaceFlinger 自带的拒绝）

```
10-06 00:12:41.654  1513  1567 E SurfaceFlinger: Permission Denial: can\'t access
    SurfaceFlinger pid=8646, uid=2000
10-06 00:12:41.710  1513  3386 E SurfaceFlinger: Permission Denial: can\'t access
    SurfaceFlinger pid=8648, uid=2000
...（每个探针一次）
```

`uid=2000` 是 shell。同样的 `service call SurfaceFlinger 1`（BOOT_FINISHED，是任何正常 shell 用户都可以发的）**也**被拒 ⇒ 不是 transaction-code 特有，是**整个 ISurfaceComposer binder 接口对 shell 拒绝**（Android 16 的 surfaceflinger 加强了 per-transaction 检查）。

## 拒因层不是 SELinux / Binder / uid

子代理交叉验证：
- `service call SurfaceFlinger 1`（BOOT_FINISHED）也拒 → 不是 1035 特有问题
- `su -c \'service call SurfaceFlinger 1035 i32 -1\'`（uid=0）也拒 → 不是 uid 问题
- 控制探针：`service call SurfaceFlinger 1033/1034/1037` 都是同样错误格式

## 对 dfps-rs 终点的影响

按 `map.md` §Destination 的降级：

1. **不再用 sf 后门**（它在本机结构性不通）
2. **回退路径**：`settings put system peak_refresh_rate 60|90|120`（小米路径，在 `SysPeakRefreshRate` 里同时 `system.peak_refresh_rate` / `system.min_refresh_rate` / `system.miui_refresh_rate` / `secure.miui_refresh_rate` 四个键一起写），或 vendor-specific 设置
3. T06 票的默认路径已切到这条

## 这个限制波及面

不只是 `SyncCallSurfaceflingerBackdoor` —— **任何走 `service call` 路径到 SurfaceFlinger 的事务都对 shell 拒绝**。意味着：

- dfps-rs 即便想用 `am instrument` / `cmd SurfaceFlinger` 之类的同等通道也是同病
- 唯一剩下的 sf 切帧率路径是 **注入器（sfanalysis 模块那条路）**或 **sysfs `peak_refresh_rate`**（在某些设备上是只读，且需 root）
- **小米设备**有 `settings put system peak_refresh_rate` 这个 manufacturer 扩展，是目前最干净的可调用路径

## 详尽档案位置

- 完整 transcript：`/tmp/sf-backdoor-probe.log`（422 行 / 95 KB；含设备状态、6 个失败探针、控制探针、logcat AVC、dmesg binder 错误、pre/post dumpsys diff）
- 完整 logcat 抓取：`/tmp/sf-backdoor-probe.logcat`（480 KB）
- 子代理的 live transcript：`/home/bigbang/.hermes/cache/delegation/live/deleg_67943d1b/task-0.log`
