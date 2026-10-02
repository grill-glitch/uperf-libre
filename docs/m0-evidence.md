# M0 实施记录（证据）

日期：2026-10-02 · 执行人：agent · 设备：alioth `f748d277`（crDroid A16 / KernelSU Next，内核 `4.19.325-cip131-st15-perf-g1899485b41b7`）

> 本文件是 M0 的**实测证据留档**。AGENT.md §10.5 要求：任何"已完成"必须附真实输出。
> 设备上的日志文件在验证结束后已清理，下方是从设备直接读回的原始片段（逐字复制）。

## 1. 构建环境（实测）

| 项 | 值 |
|---|---|
| NDK | `~/Android/Sdk/ndk/android-ndk-r30`（r30, 16248370）→ `aarch64-linux-android23-clang` |
| CMake | 4.4.3 · GNU Make 4.4.1 · Rust 1.94.0-nightly（M1 起用） |
| dfps vendor | `f84866c1ff1518da72037056844cb0917a941904`，210 文件，`diff -rq` 与上游工作树一致（0 处差异） |
| 构建命令 | `sh build.sh Release make check` |

构建过程中为兼容 NDK r30（clang 21）所做的**非侵入式**处理（未改动 `cpp/dfps/**` 任何字节）：

1. `cpp/CMakeLists.txt` 对 scnlib 加 `-include memory`：scnlib 的 `detail/util.h` 用了 `std::addressof()` 却没 include `<memory>`，NDK r24 的 libc++ 头恰好间接引入，r30 不再引入 → 编译失败。
2. `cpp/CMakeLists.txt` 把 spdlog / scn 的 include 根标成 `INTERFACE_SYSTEM_INCLUDE_DIRECTORIES`：spdlog 1.9.2 内置 fmt 的 `operator"" _format`（带空格）在 clang 21 下告警，而 dfps 的标志里有 `-Werror` → 系统头豁免告警。
3. `cpp/uperf/CMakeLists.txt` 的 Release 链接加 `-static-libstdc++`：dfps 用 `-static`（全静态），但原版 `magisk/bin/uperf` 是 **动态 PIE**（`NEEDED = libm/libdl/libc`），不加该标志时驱动默认链 `libc++_shared`，而 Android 上没有这个库。

## 2. 产物断言（`build.sh check` 原始输出）

```
>>> Checking /home/bigbang/uperf-rewrite/build/aarch64-linux-android23/runnable/uperf
    size      : 586232 bytes (.55 MiB)
    NEEDED    : libc.so libdl.so libm.so 
    .interp   : 1
    .symtab   : 0
    RELRO     : GNU_RELRO  BIND_NOW: 1
    file: .../runnable/uperf: ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV),
          dynamically linked, interpreter /system/bin/linker64, for Android 23,
          built by NDK r30 (16248370), stripped
    -> OK
```

与原版 `dev-22.09.04` 二进制对照（原版 1,461,512 B，同样 `NEEDED libm/libdl/libc` + `BIND_NOW` + `NOW PIE`）：
**外部依赖与 ELF 形态一致**；体积差异来自尚未链接的策略模块（M1 起才有）。
拒绝的依赖（`libc++_shared.so`）是这次唯一一次 `check` 失败，已修（见 §1.3）。

## 3. 真机运行（`/data/local/tmp/uperf <stub.json> -o <log>`，root 下运行）

进程树（fork 监督器已生效，daemon → app child）：

```
 11945  1845 [uperf]      ← 非本进程，内核线程，名字巧合
 11947     1 uperf        ← daemon（fork 后父进程退出，setsid）
 11948 11947 uperf        ← app child
```

日志（逐字，取自设备 `/data/local/tmp/uperf_m0_log.txt`）：

```
19:35:42 I uperf m0(rs-rewrite)[f351adf], by grill-glitch (Rust rewrite project)
19:35:42 I uperf[f351adf] M0 platform bring-up, config=/data/local/tmp/uperf_m0_stub.json log=/data/local/tmp/uperf_m0_log.txt (config parsing lands in M2)
19:35:42 I EventTap: subscribed to 13 topics
19:35:42 I Uperf is running
19:35:43 I EventTap: cgroup.ta.list = 71 pid(s) [1988 1989 1990 2208 2219 2220 3771 4638 ]
19:35:43 I EventTap: cgroup.fg.list = 1182 pid(s) [701 705 706 717 730 731 732 744 ]
19:35:43 I EventTap: cgroup.bg.list = 782 pid(s) [2169 2696 2698 3106 3109 3115 3116 3117 ]
19:35:43 I EventTap: input.touch = true
19:35:43 I EventTap: input.state = hold:true swipe:false gesture:false
19:35:43 I EventTap: input.touch = false
19:35:43 I EventTap: input.state = hold:false swipe:false gesture:false
19:35:43 I EventTap: topapp.pkgName = org.librelab.messaging
19:35:44 I EventTap: cgroup.ta.list = 99 pid(s) [...]
19:35:44 I EventTap: cgroup.re.list = 0 pid(s) []
19:36:06 I Config file updated, restart uperf to load new config file
19:36:06 I uperf[f351adf] M0 platform bring-up, config=/data/local/tmp/uperf_m0_stub.json ...
19:36:06 I EventTap: subscribed to 13 topics
19:36:06 I Uperf is running
19:36:06 I EventTap: input.touch = true
19:36:06 I EventTap: input.state = hold:true swipe:false gesture:false
19:36:09 I EventTap: cgroup.ta.list = 71 pid(s) [...]
19:36:10 I EventTap: topapp.pkgName = org.librelab.messaging
```

CLI（无参数，原始输出）：

```
19:36:10 E Config file not specified

Userspace performance controller for Android 6.0+. Details see https://github.com/yc9559/uperf.
Usage: uperf [-o log_file] config_file
```

### 3.1 验到的事实

| 验收点 | 结果 | 证据 |
|---|---|---|
| 进程监督器（fork + setsid + 进程名 `uperf`） | ✅ | `ps` 里 `11947 daemon → 11948 child`；日志 `Uperf is running` |
| 日志格式 = dfps 的 `%H:%M:%S %L %v` | ✅ | 所有行 `HH:MM:SS I <msg>`，无方括号（与 §7.3 的静态推断一致） |
| 事件总线 + 4 个事件源 | ✅ | `cgroup.{ta,fg,bg,re}.list` 计数随设备负载变化；`input.touch`/`input.state` 捕获到**真实触摸**；`topapp.pkgName = org.librelab.messaging`（走 `GetTopAppNameDumpsys`） |
| 配置热重载（inotify CLOSE_WRITE → SIGUSR1 → 重启 app child） | ✅ | 改写 config 后出现 `Config file updated, restart uperf...`，child pid `11948 → 12288`（daemon 11947 不变） |
| CLI 契约 | ✅ | 无参数打印错误 + 与原版**逐字相同**的帮助串；`<config> -o <log>` 生效 |
| 崩溃兜底（tombstone） | 未测 | 需要人为触发 SIGSEGV；M2 视需要补 |
| `offscreen.state` | ❌/未定 | 见 §3.2 |

### 3.2 未验到 / 反例（必须带入 M1-M2 的清单）

1. **`offscreen.state` 在 alioth 上没触发**：`/dev/cpuset/restricted` 的 pid 数**恒为 0**（background 37、top-app 3~223 都在动），而 vendored `OffscreenMonitor` 的判据是 `restricted > 10`。
   * 但本次没能真正熄屏：`adb shell input keyevent 26` 之后 `dumpsys power` 仍为 `mWakefulness=Awake`，所以**不能断定**是"ROM 不用 restricted"还是"这次没熄屏"。
   * 待办（M2）：① 用物理电源键熄屏后读 `/dev/cpuset/restricted/cgroup.procs`；② 用**原版二进制**在同一台机同一时刻抓 `offscreen` 相关日志，确定 uperf v3 的真实判据/数据源。
   * 这不是小问题：`switcher` 的 `-`（熄屏）分支、`junk` 检测都挂在熄屏状态上。
2. `input.*` 的完整分类（`swipe` / `gesture`）本次只看到 `hold:true`；需要真机做滑动/全面屏手势各一次（M2 起按 §10.4 场景采集）。
3. `topapp.pkgName` 走的是 dumpsys 且带 `|Δpid| > 10` 门槛（`TOP_TASK_NR_DIFF_MIN`），本次是靠 top-app 从 71 涨到 99 才触发；小应用切换不触发属正常，但**这正是 uperf 的 `perapp` 判据**，M2 必须确认原版是否用同一门槛。
4. `/dev/cpuset` 在本机是 **cgroup v1 形态**（存在 `tasks`/`notify_on_release`），而 `/sys/fs/cgroup` 是 v2（只有 `apps`/`system`）。两条路径同时存在，M2 要把"哪个是真正的任务分组视图"钉死，否则 cgroup 事件会读到陈旧数据。

## 4. 清理

设备侧：`pkill -x uperf` → `ps` 已无 `uperf`；`/data/local/tmp/{uperf,uperf_m0_stub.json,uperf_m0_log.txt}` 已删除。
仓库侧：未触碰 `magisk/bin/uperf`（原版二进制仍在，作为 parity 参照）。
