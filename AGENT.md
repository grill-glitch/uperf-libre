# AGENT.md — Uperf 重写：契约、不变量与验收

> 本文只写**当前**成立的东西：契约（§5/§7）、架构不变量（§6/§8）、验收（§10）、规则（§13）。
> 历史决策、逐里程碑证据、已解决的未知项在 [`docs/history/`](./docs/history/)。
> 改代码前读 §6 与 §13；改 shell 前读 §4；动构建读 §9。

> 本文件是本仓库的**施工说明与验收标准**。任何在本仓库工作的 agent 必须先读完本文件，
> 并在提交时对照第 10 节的验收条目自查。**禁止把"应该能"写成"已验证"。**

- 仓库：`grill-glitch/uperf-libre`（fork，网络根 `yc9559/uperf`）
- 内容基线：`yinwanxi/Uperf-Game-Turbo` @ `b13d54a`（= Uperf v3 `dev-22.09.04` 二进制 + 63 份平台配置 + 平台集成脚本）
- 上游同步分支：`master`（== `upstream/master`，只用于同步，不受本项目改动）
- 本项目工作分支：`game-turbo`（默认分支）

---

## 1. 目标与边界

**目标**：把仓库里那个**闭源二进制** `magisk/bin/uperf` 替换为自研实现。
被重写的业务部分用 **Rust**；已经由作者同源开源项目 **dfps** 公开的 C++ 平台层**直接复用，不重写**。

**非目标**：
- 不改 `config/*.json` 的语义与数值（63 份配置是资产，只允许新增不允许改数）。
- 不改 `magisk/*.sh` 的调用契约（除第 7 节允许的最小适配）。
- 不追求与闭源二进制逐指令等价；追求**配置语义等价 + 运行时行为等价**（用 §10 的 parity 工具证明）。

**0 闭源 blob 不变量**：模块发布的二进制树里**不得包含任何闭源 `.so`**。SfAnalysis 的注入库由本仓库
重写（`uperf-sfanalysis` → `libsfanalysis_rs.so`），vendor 的 `libsfanalysis.so` 已从产物中移除，
`build.sh check` 会断言这一点。机制与边界见 §12.1 与 `docs/spec/sfanalysis.md`。

**成功判据（每条都要有可复现的证据）**：

1. `magisk/bin/uperf` 是自研构建，模块在 alioth（crDroid A16 / KernelSU Next）开机自启，
   `uperf_log.txt` 无 `[E` 级别错误（§10.3 的装机/开机脚本）。
2. 63 份 `config/*.json` 全部解析，且**告警/忽略行与上游逐字一致**（§10.2 的 parity 工具）。
3. `modules.sysfs.enable: true` 的配置在 fake root 镜像上的写入序列与参考一致（§8.3 + §10.2）。
4. 停机路径干净：`killall uperf` 后无残留进程、`userspace` 接管已恢复、`state=stopped`（§10.3）。
5. 发布产物 0 闭源 blob（`build.sh check`）。

---

## 2. 设备与运行契约（当前约束）

只列**仍然约束代码**的事实；每条的量测过程、证据链与当时结论在
[`docs/history/device-findings.md`](./docs/history/device-findings.md)。

| 约束 | 后果 |
|---|---|
| alioth 的 `scaling_max_freq` 是**驱动级只读**（0444，root 写也 EACCES，且 `[ -w ]` 因 `CAP_DAC_OVERRIDE` 会骗人） | 调频只能 `scaling_governor=userspace` + `scaling_setspeed`；写前判 mode 位，不判 `access(2)`。见 §8.4 |
| `/sdcard`（FUSE）下 root `unlink` 会**静默成功但文件仍在** | 删除走 `/data/media/0/...`（同步清 FUSE 视图） |
| 守护进程与它的 worker **都叫 `uperf`**（dfps 改写自己的 cmdline），`pidof` 也不匹配 zombie | 停服用 `killall uperf`；找活体用 `/proc/<pid>/exe`，不用名字/pid 文件 |
| 模块路径 `.` = `/data/adb/modules/uperf`；`USER_PATH=/sdcard/Android/yc/uperf`；CLI 契约 `uperf <cfg> -o <log>` | 见 §7 |
| `setup.sh` 安装时把 `config/<soc>.json` 播种成 `USER_PATH/uperf.json`（旧文件备份 `.bak`），然后删掉模块里的 `config/` | 装机后比对 zip 与模块目录时必须允许 `config/` 缺席（`scripts/install-and-verify-staged.sh`） |
| 设备上没有 `patchelf`；SF 注入要求库落在带 `system_file` 标签的路径且显式 `UPERF_SF_INJECT=1` | 默认只装库不碰 surfaceflinger |
| 本机 SELinux（Enforcing）拒绝 `execmem`/`execmod`，KernelSU-Next 也无可用 sepolicy 通道 | SF 侧的 hook 只能走 **GOT/PLT 改写**，不能 inline hook |
| `modules.sysfs.enable` 出厂 63 份配置全为 `false` | 真实 sysfs 写入是显式 opt-in；`RealSink` 存在但默认不跑（§8.3） |
| **已知内核缺陷**：KSU-Next 的模块挂载 + 本内核会在关机阶段触发 dcache panic（`list_del corruption` → `__dentry_kill`），`sys.boot.reason=kernel_panic,bug`；panic 后 f2fs 可能丢最近写入的块 | 每次开机后必须核对模块文件 md5 与全 NUL 扫描（`scripts/post-boot-verify.sh`）；不要假设 boot 是干净的 |
| `/proc/sys/fs/inotify/*` 可写（`max_queued_events`/`max_user_watches`），但 `chown/chmod` 在 ksu 域 EACCES | 调参 best-effort，失败只报一行 |

## 3. 架构与选型

### 3.1 分层

```
┌──────────────────────────────────────────────────────────────┐
│ C++ 侧（复用 dfps，Apache-2.0，占进程骨架）                    │
│  main.cpp        进程监督器：fork 子进程 / SIGCHLD 拉 tombstone│
│                  SIGUSR1 优雅重启（配置热重载）/ 日志(spdlog)   │
│  platform/       ModuleBase(事件总线) CoBridge DelayedWorker   │
│                  HeavyWorker Inotifier Singleton              │
│  modules/        InputListener CgroupListener OffscreenMonitor │
│                  TopappMonitor           ← 事件源，逐字复用     │
│  utils/          inotify input_reader sched_ctrl atrace        │
│                  misc_android backtrace time_counter          │
└───────────────▲──────────────────────────┬───────────────────┘
                │ extern "C" 桥（双向）      │ 事件回调
                │ cpp/include/uperf_rs.h    │
┌───────────────┴──────────────────────────▼───────────────────┐
│ Rust 侧（本项目重写，staticlib `libuperf_rs.a`）               │
│  uperf-config  schema/层叠/告警 + sysfs 规划 + governor + sched │
│  uperf-core    daemon(任务注册表) + orchestrator + 各任务 + C ABI│
│  uperf-dfps    动态刷新率（独立 crate/subtree）                  │
│  uperf-sfanalysis 注入库（cdylib，独立 profile）                 │
│  uperf-cli     主机端 parity 工具（不发布）                      │
└──────────────────────────────────────────────────────────────┘
```

**决策（已定，勿再反复）**：**C++ 拥有 `main()` / 进程骨架 / 日志 / 事件总线；Rust 以 staticlib 被链接，承担全部被重写的业务模块。**
理由：`main.cpp` 的 fork+SIGCHLD+tombstone+SIGUSR1 监督器是最容易写错的一块，dfps 已验证可用；把它留在 C++ 既最大化复用，又让"重写部分 = Rust"的边界清晰。

### 3.2 Rust 依赖（保持最小）

| crate | 用途 | 备注 |
|---|---|---|
| `serde` + `serde_json` | 配置解析 | 不允许换成手写 parser |
| `regex` | `sched` 规则正则 | **不需要 PCRE2**：63 份配置的 28 个唯一模式全是普通 ERE（无 lookaround/atomic/`\K`/反向引用/POSIX word-boundary），`uperf-config/tests/all_sched_configs.rs` 会拒绝新增的 PCRE-only 语法。见 §8.5 |
| `libc` | syscall/inotify/prctl/affinity | |
| `log` + 自写 formatter | 日志 | 输出格式必须与原版一致，见 §7.3 |
| `thiserror` | 错误类型 | 禁止 `unwrap()` 于运行路径 |
| `once_cell` / `parking_lot` | 全局态与锁 | 与 C++ 侧共享状态用 `Mutex`，禁止无保护的 `static mut` |

不引入 async runtime：原实现是"线程 + 轮询 + inotify"，改成 tokio 会带来无法对账的时序差异。

---

## 4. 目录结构（目标）

```
/config/                    63 份平台配置（冻结，勿改）
/magisk/                    模块骨架；只需把 bin/uperf 换成自研产物
  customize.sh service.sh post-fs-data.sh uninstall.sh   ← 只放 Magisk/KernelSU 按名字直接调用的入口
  script/                  ← 其余全部在script/：initsvc.sh、libuperf.sh、libcommon.sh、libsysinfo.sh、
                             libcgroup.sh、libpowercfg.sh、pathinfo.sh、powercfg_{main,once}.sh、
                             miui_migt.sh、platform_special.sh、vtools_powercfg.sh、webui.sh、dfps.sh、
                             setup.sh、uperf_watchdog.sh
                             （`build.sh check` 断言根目录只剩 4 个入口、script/ 一个不少——
                              历史上根目录曾有一份会漂移的副本，包括 governor 恢复路径）
/cpp/
  dfps/                     ← vendor 自 yc9559/dfps（Apache-2.0），保留原文件头
    platform/ modules/ utils/ main.cpp CMakeLists.txt
    DFPS_VENDOR.md          vendor 记录：上游 commit / 文件清单 / 本地改动清单
  uperf/
    app_main.cpp            main.cpp 的 uperf 化（PROC_NAME="uperf"、日志 pattern、CLI）
    bridge.cpp              extern "C" 桥：订阅转发、worker/sched/misc 封装
    include/uperf_rs.h      给 Rust 看的 C 头（与 cbindgen 产物对齐）
    CMakeLists.txt
/rust/
  Cargo.toml                workspace（5 个 crate）
  uperf-config/             schema/层叠/告警 + sysfs 规划 + governor + scheduler（无 unsafe）
  uperf-core/               daemon/任务/orchestrator/FFI（staticlib）
  uperf-dfps/               动态刷新率（独立 crate，git subtree）
  uperf-sfanalysis/         SF 注入库（cdylib，独立 profile）
  uperf-cli/                纯 Rust 工具（离线配置/parity 用，不随模块发布）
/docs/                      索引见 docs/README.md
  spec/                     契约与规格（config-v3、sfanalysis）
  research/                 上游/第三方调查
  records/                  逐里程碑的设计、证据与设备跑批日志（m0..m11）
  history/                  项目史：里程碑汇总、设备事实、已解决的未知
  audit/                    unsafe 审计报告
  configs/{upstream,ugt}/   38 份上游基线配置 / 63 份 UGT 配置（只读快照）
/build.sh                   make / pack / install / reboot / clean / check
```

`cpp/dfps/**` 一旦 vendor **不得修改**；需要改动就复制到 `cpp/uperf/` 并在 `DFPS_VENDOR.md` 记录。

---

## 5. FFI 契约

### 5.1 事件载荷（C++ → Rust）

⚠️ 原总线直接传 C++ 对象（`std::string*`、`std::vector<int>*`），**禁止让 Rust 直接解引用 STL 对象**——那等于把 libc++ ABI 焊死。桥接层必须在 C++ 侧转换后再跨边界。

| topic | C++ 载荷 | 桥接后（C ABI） | Rust 侧类型 |
|---|---|---|---|
| `cgroup.{ta,fg,bg,re}.list` | `PidList*`（`vector<int>`） | `const int32_t* + size_t` | `&[i32]` |
| `cgroup.{ta,fg,bg,re}.update` | `nullptr` | 无载荷 | `()` |
| `input.touch` | `bool*` | `bool` | `bool` |
| `input.btn` | `bool*` | `bool` | `bool` |
| `input.state` | `InputData*`（3×bool） | `#[repr(C)] struct{bool,bool,bool}` | `InputData` |
| `topapp.pkgName` | `std::string*` | `const char*`（NUL 结尾，生命周期=调用内） | `&str` |
| `offscreen.state` | `bool*` | `bool` | `bool` |
| `sfanalysis.hint` | 文件协议（见 §7.4） | — | Rust 侧 inotify/读文件 |

### 5.2 Rust 导出（C++ 调用）

```c
// cpp/include/uperf_rs.h
int  uperf_rs_start(const char *config_path, const char *log_path);  // 0=成功
void uperf_rs_stop(void);                                           // 收到 SIGUSR1
void uperf_rs_reload(void);                                         // 配置热重载
void uperf_rs_on_event(const char *topic, const void *data, size_t len);
```

### 5.3 Rust 导入（桥到 C++ 平台）

```c
// 订阅事件（内部转成 C++ 回调 → uperf_rs_on_event）
void uperf_bridge_subscribe(const char *topic);
// Worker
uint64_t uperf_bridge_dw_create(const char *name);
void     uperf_bridge_dw_set(uint64_t handle, void (*cb)(void*), void *ud, int64_t ts_us);
uint64_t uperf_bridge_hw_create(const char *name);
void     uperf_bridge_hw_set(uint64_t handle, void (*cb)(void*), void *ud);
// 工具
int  uperf_bridge_sched_set_prio(int tid, int prio, bool reset_on_fork);
int  uperf_bridge_sched_set_affinity(int tid, const uint8_t *cpumask_bytes, size_t n);
int  uperf_bridge_sched_set_class(int tid, int policy, int prio);
int  uperf_bridge_screen_brightness(void);
int  uperf_bridge_os_version(void);
void uperf_bridge_set_thread_name(const char *name);
```

`uperf_rs.h` 必须与 `cbindgen` 生成结果一致，CI 里跑 `cbindgen --check`。

---

## 6. 模块归属表

| uperf 模块 | 归属 | 说明 |
|---|---|---|
| 进程骨架 / 日志 / 事件总线 | **C++（dfps）** | `main.cpp` + `platform/*`，仅改 `PROC_NAME`、日志 pattern、CLI |
| InputListener / CgroupListener / OffscreenMonitor / TopappMonitor | **C++（dfps）** | 逐字复用，topic 不变 |
| inotify / input_reader / sched_ctrl / atrace / misc_android / backtrace / time_counter | **C++（dfps）** | 逐字复用 |
| 模块装配（原 `uperf.cpp`） | **Rust** | `rust/uperf-core/src/lib.rs` |
| 配置管理（原 JSON/knob 体系） | **Rust** | `config/`，含层叠覆盖与告警文案 |
| `switcher`（hint 状态机 + preset + perapp） | **Rust** | `switcher/`，状态机见 §8.1 |
| `profile`（scene → 参数表下发） | **Rust** | `profile/` |
| `sysfs` 写入器 | **Rust** | `uperf-config/src/sysfs.rs`（按路径推断的 5 类 + `payload()`）+ `uperf-core/src/{orchestrator,real_sink}.rs`，见 §8.3 |
| `cpu` 调频器（governor + 负载采样） | **Rust** | `governor/`，见 §8.4 |
| `sched` 上下文调度器 | **Rust** | `sched/`，见 §8.5 |
| `sfanalysis` 监听 | **Rust** | `sf/`，见 §7.4 / §12.1 |
| `anim_watcher` / `log_level_switcher` / `atrace_switcher` | **Rust** | `anim/`、`logmod/` |
| `CpufreqWriter{Msm,Ppm,Epic}{Base,Fixed,Powersave}` + `Performance` | **Rust** | 平台差异（`/sys/kernel/msm_performance/...`、`/proc/ppm/policy/...`、`/dev/cluster{}_freq_{min,max}`）在 Rust 里用策略表表达 |
| `libsfanalysis.so`（SF 注入库）→ `libsfanalysis_rs.so` | **Rust 重写** | 见 §12.1 / `docs/spec/sfanalysis.md`；vendor 件已不再随模块发布（`build.sh check` 断言 0 闭源 blob） |
| 外置看门狗 / 状态文件（M9） | **shell + Rust** | `magisk/script/uperf_watchdog.sh`（进程外监督 + 死手恢复 + 预算内重启）、`rust/uperf-core/src/status.rs`（`uperf.state`）；见 §11 M9 与 `docs/records/m9-watchdog.md` |
| 运行时旋钮（无上游对应物） | **Rust** | `rust/uperf-core/src/settings.rs`：全部 `UPERF_*` 环境变量在此**一次性**解析成 `Settings`，下游模块只接受参数，不再自己 `std::env::var`。新增旋钮只改这一处 + 传参，见 §13 |
| 守护进程装配 / 任务生命周期（无上游对应物） | **Rust** | `rust/uperf-core/src/daemon.rs`（谁存在、按什么顺序起停）、`task.rs`（`Task` trait + `Registry`）。停止顺序由注册顺序推导（逆序），**唯一例外**是 CPU governor 按名字先停：userspace 接管是唯一不可由进程内恢复的状态 |

### 6.0 任务生命周期（`Task` / `Registry` / `Daemon`）

新增一路数据源，以前要改五处（static、slot 访问器、start 块、stop 块、任务文件），而 stop 列表是手写的、
编译器不管——漏一个就是线程活着而 daemon 宣称 `state=stopped`，CPU governor 留在 `userspace`
（本项目唯一不可自恢复的状态）。现在：

* 任务实现 `Task`（`name` / `stop` / `is_running`），注册进 `Registry`；`stop_all` 按**逆序**停，
  每步 `is_running()` 断言并把 `task '<name>' stopped` 写进日志（泄漏可见）。
* `Daemon` 独占 `Settings` / orchestrator / dfps / dispatcher / `Registry`；`uperf_rs_start` 只负责
  解析 Settings、写 `uperf.state`、`Daemon::start`。新增子系统 = `daemon.rs` 里一个 stage 函数 + 一次 `push`。
* 所有 stop 一律走 `shutdown::stop_and_join`（有界 join）。`recorder`/`ctl`/`sf-uprobe`/`sf-binder`
  原先是无界 `join()`，一个卡住的线程就能把停机路径（也就是 disarm 路径）挂死。
* 事件通道的 sender 现在可替换：再次 start 时旧 receiver 已死，`OnceLock` 会让其后所有事件静默丢进死通道。

### 6.02 停机信号（`signals.rs`）

* 停机信号（SIGTERM / SIGINT / SIGUSR1）在 worker 里**先屏蔽再起线程**（`cpp/uperf/app_main.cpp::BlockTerminationSignals`，在 fork 的子进程分支里、`StartPlatform()` 之前），由一个 `sigwait` 线程在**普通线程上下文**里调用 `uperf_rs_stop()`，然后 `_exit(0)`。全部线程继承屏蔽字，所以第二次投递不会打断正在进行的停机（这正是 `killall uperf` 过去把停机路径在同一线程上跑两遍、撞死非重入日志锁的形状）。
* **禁止在信号处理器里调用 `uperf_rs_stop()`**：它 join 线程、取 `parking_lot`/`std` 锁、分配内存、写状态文件、走 spdlog，全都不是 async-signal-safe。信号可能落在正持有这些锁的线程上——dispatcher 里 C++ spdlog sink 锁没有 `try_lock` 版本；cpu task 里 `CpuTask::stop` 超时回退会在同一线程再取 `writer` 锁。任一种都让进程不退、governor 留在 `userspace`。
* 监听线程只在 daemon 已放进槽位**之后**启动：否则启动途中来信号会 `_exit` 在「governor 已 arm、daemon 未发布」的窗口里。
* `Dispatcher::join_timeout` 必须先 `ffi::clear_dispatch_tx()`：FFI 槽位持有 sender，只 drop `_keep_alive` 时 receiver 永远收不到 `Disconnected`，线程会白等满 2 s 预算（真机实测：`caught signal` 与 `task 'cpu' stopped` 相差 2 s）。

### 6.03 帧源阶梯（`frame_state.rs`）

三条腿：注入 hint（M8）> uprobe（⑪）> binder `--latency`（⑤）。阶梯、各腿最后读数、状态文件
`uperf_frames.state` 的**唯一写者**现在都在 `frame_state`：

* 各腿只调用 `publish_uprobe()` / `publish_binder()` / `publish_hint()` 上报；读的一方只问
  `active()`（hint 在看 hint、否则 uprobe 健康就看 uprobe、否则 binder）或 `last_fps()`（⑧ 记录用）。
  以前阶梯住在 `sf_binder`（降级腿）里，还要伸手读 `sf_uprobe` 的模块级 static——加第四条腿要改降级腿的主循环。
* hint 在生效时不带 fps（一个字节的 hint 不是计数），所以 `active()` 此时返回 `source=hint, fps=0`——
  与记录器此前的行为一致，只是从散落的 `set_last_fps(0.0)` 收敛到一处。
* 状态文件字段由 `Published::render()` 一处生成，测试钉住 WebUI 解析的每个 key（含"缺失值渲染成 `-` 而不是 0"）。

### 6.04 控制面（三条，各归各位）

| 面 | 实现 | 谁在用 | 说明 |
|---|---|---|---|
| `webui.sh` 子命令 | `magisk/script/webui.sh`，输出 `key=value` | **WebUI 唯一后端**（`webui/ctl.js`）+ adb + 全部 e2e | KernelSU 的 WebUI 只能 `exec`，这条路无法替代 |
| ctl socket | `rust/uperf-core/src/ctl_socket.rs`，反连 + token/version/pid 握手，命令集 `PING`/`PONG`/`STATUS` | `tools/ctl-listen`（参考消费者） | 同域对端（root/ksu）用；`STATUS` 回的是状态文件内容，好让两边不可能不一致 |
| 状态文件 | `uperf.state` / `uperf_watchdog.state` / `uperf_frames.state` | 看门狗、`webui.sh`、WebUI | 只读视图；每个文件的写入方各只有一个 |

* 状态文件的 `key=value` 读取在 shell 侧收敛成 **`libcommon.sh::state_get`**（`webui.sh` 里 12 处 `sed -n 's/^k=//p'` 全部换成它）。Rust 侧写入点是 `status.rs` 与 `frame_state::Published::render()`，所以字段改名只有两个写入点 + 一个 shell 读取点。
* ctl socket 的 `handshake()` **必须复用连接上同一个 `BufReader`**：对端常在读到 `OK` 后立刻发下一条（`STATUS`），握手若自建 reader，那几条字节会留在它的缓冲区里随它一起被丢弃——这个缺陷是给这条控制面加第二个命令时才暴露的。
* 不要在 `webui.sh` 里加 `stop`/`start`：模块的启停语义在 `libuperf.sh`（`uperf_start` / `uperf_stop`，含看门狗停止与 governor 恢复），`webui.sh restart` 已经是那套的入口。

### 6.1 运行时旋钮面（`Settings`）

`uperf_rs_start` 在最外层调用 `Settings::from_env(config, log)`，派生出 `uperf.state` / `orig_governor.txt` /
`sysfs_orig.txt` / `uperf_frames.state` / `uperf.token` 的路径，然后**传值**给每个任务。

* 不允许在模块内部读环境变量——那会让"这个 daemon 有哪些旋钮"无法回答，也让测试必须改进程环境。
  `std::env::set_var` 在多线程进程里是对 `environ` 的数据竞争（Rust 2024 已标 `unsafe`），本项目的
  `uperf_rs_start` 曾在 reload 时踩到这一点，现已改为纯派生。
* 测试构造 `Settings` 直接注入（见 `cpu_task` 的 `scoped_writer`、`sched_task` 的 `Settings::default()`），
  不再有"串行化 env 的全局锁"。
* 仍留在环境里的：`sfanalysis` 注入库自己的 `UPERF_SFANALYSIS_*`（它是另一个 cdylib，不共享本 crate 的 Settings）。

---

## 7. 兼容性契约（不可变更清单）

### 7.1 CLI

```
uperf <config.json> [-o <log_file>]
```
* 进程名 = `uperf`（`killall uperf` 必须能停）。
* 启动后 2s 内必须完成初始化（脚本 `sleep 2` 后做 cgroup 迁移）。
* 无参数时打印含 `Usage: uperf [-o log_file] config_file` 的帮助并退出。

### 7.2 兼容的开关文件（外部依赖，不得改路径）

| 路径 | 语义 |
|---|---|
| `modules.switcher.switchInode`（默认 `USER_PATH/cur_powermode.txt`） | inotify 监听；写入 preset 名（或 `auto`）切换模式 |
| `modules.switcher.perapp`（默认 `USER_PATH/perapp_powermode.txt`） | 分 APP 模式；`*`=默认、`-`=熄屏，**必须存在**，大小写敏感全字匹配 |
| `USER_PATH/uperf_log.txt` | 日志（`-o` 指定） |
| `flag/need_recuser` | 启动自恢复标记（脚本侧创建/删除，仓库不提交该目录）；本项目不改 |

### 7.3 日志格式（**已核验：README 的示例是 v1 时代，不能用**）

证据链：
* v3 二进制里存在的 pattern 串只有 `%H:%M:%S %L %v`（与 dfps `source/main.cpp:InitLogger()` 的
  `logger->set_pattern("%H:%M:%S %L %v")` 逐字相同），**没有** `[%H:%M:%S][%L]` 这种带方括号的串。
* v3 二进制里**不存在** `CfgMgr` 这个字符串；存在的是模块名 `CpuGovernor` / `SysfsWriter` /
  `ContextScheduler` / `ProfileSwitcher` / `SfAnalysisListener` / `TopappMonitor` / `CgroupListener` /
  `InputListener` / `AtraceSwitcher` / `AnimWatcher`。
* v1 二进制里反而是 `CfgMgr: Using [%s] by [%s]` 这种把标签写进消息的 printf 风格串。
* README（本仓库与上游同款）的日志示例用的还是 `/sdcard/yc/uperf/`（v2 路径），而 v3 的 USER_PATH
  是 `/sdcard/Android/yc/uperf`。

结论：**v3 的日志行形如 `13:03:33 I <消息>`**（spdlog 默认 sink + `%H:%M:%S %L %v`），
标签由消息内容或 logger 名承载。M0 已按此 pattern 落地（`cpp/uperf/app_main.cpp:InitLogger`）。
**精确到"哪条日志由哪个 logger 名产出"仍未静态确定 → M2 起以真机原版日志逐条采集为准。**

必须保留的关键行（parity 断言的锚点，均已在本机二进制里核到原串）：
```
uperf v3(22.09.04)[<hash>], by Matt Yang (yccy@outlook.com)   （守护进程横幅，格式串 "{} {}[{}], by {}"）
Uperf is running
Config file updated, restart uperf to load new config file
Failed to start uperf(pid=...)
uperf(pid=...) terminated unexpectedly, try to get tombstone
>>> Start of tombstone ... <<< / >>> End of tombstone ... <<<
Cannot find the tombstone
Usage: uperf [-o log_file] config_file
```
> ⚠️ 配置层的日志文案（`Using [..] by [..]`、`Ignored root/platform/knobs/..` 等）在 v3 里**不存在同名串**，
> 说明 v3 的配置层重写过，具体文案必须真机采集，不许照抄 v2 README。

### 7.4 `sfanalysis.hint` 文件协议

SF 侧注入库向外部写单字节状态（原库实现：`lseek+read/write` 单字节状态机，状态码 0..3）。
Rust 侧必须：
1. 按 `modules.sfanalysis.enable` 与模块 flag 决定是否启用；
2. 监视 hint 文件（inotify）；
3. 把状态映射到「渲染开始 / 渲染滞后 / 渲染结束」，并按 `renderIdleSlackTime` 判定渲染结束；
4. 在渲染结束信号后 66ms 内结束 Hint（README 所述行为基线）。
**状态码到语义的映射已静态推导**（6 值 idle/switch/trigger/gesture/touch/junk ↔ 0..5，≥6 = unknown）：
见 [`docs/records/m1-static-reverse.md`](./docs/records/m1-static-reverse.md) §1.3，实现见 §8.1。

---

## 8. Rust 侧实现要点

### 8.1 hint 状态机（`config/README.md` 的 mermaid 是规范）

```
[*] -> idle
idle    -> touch    : 按下触摸/按键
idle    -> switch   : 亮屏
touch   -> trigger  : 抬手 / 开始滑动
touch   -> gesture  : 检测到全面屏手势
touch   -> switch   : 检测到窗口动画
touch   -> junk     : 检测到掉帧（sfanalysis）
gesture -> switch / junk
junk    -> touch    : 超时 / 结束
trigger|gesture|switch|touch -> touch : 超时或渲染结束
touch   -> idle     : 超时或渲染结束
```
每个 hint 有最长时长（`modules.switcher.hintDuration.{idle,touch,trigger,gesture,switch,junk}`，
`idle` 为 0.0 表示默认无时限）；`junk` 时长取 **preset/scene 同名键**（`junk` 场景）。

### 8.2 配置层叠（**已按 38 份真实配置修正：不是 `base_hint` 嵌套**）

实测的 v3 结构（`docs/configs/upstream/*.json` 全量验证，见 `docs/records/m2-evidence.md`）：

```jsonc
{
  "meta":     { "name": "...", "author": "..." },
  "modules":  { "switcher": {...}, "atrace": {...}, "sfanalysis": {...},
                "sysfs": {...}, "sched": {...}, "cpu": {...}, "anim": {...},
                "input": {...}, "log": {...} },
  "initials": { "<mod>.<param>": <value>, ... },     // 扁平点号键
  "presets":  { "balance": { "*": {...}, "idle": {...}, "touch": {...},
                             "trigger": {...}, "gesture": {...}, "switch": {...},
                             "junk": {...} }, ... }  // 每个 preset 下：scene -> {点号键: 值}
}
```

**关键更正**：`presets.<preset>` 的下一层**直接就是 scene 名**（`*` / `idle` / `touch` /
`trigger` / `gesture` / `switch` / `junk`），`*` 是该 preset 的通配默认。**38 份配置里没有任何
一份使用 `base_hint` 或 `presets.<p>.initials` 嵌套**（AGENT.md 早先版本记错了，
来自对文档而非数据的推断）。层叠优先级：

1. `presets[<mode>][<scene>][<dotted>]`
2. `presets[<mode>]["*"][<dotted>]`
3. `initials[<dotted>]`

`modules.*` 为静态段，只在实例化时读一次。**必须复刻原版的告警语义**（§10.2 逐条对账）。

### 8.3 sysfs 写入器（**已按 `modules.sysfs.knob` 实测重写**）

**路径不来自代码，来自配置**：`modules.sysfs.knob` 是 `{knob名: 绝对路径}` 表。
上游二进制里**没有**任何 `devfreq`/`llcc`/`ufshc` 字符串（`strings` 0 命中），
所以 `uperf-cli plan` 与设备侧都从这张表查路径（见 `docs/records/m4-evidence.md` §1）。

写入器种类**由路径形状推断**（schema 里没有 `type` 字段，README 的 type 列是 v1/v2 遗留）：

| 路径形状 | WriterKind | 值格式 |
|---|---|---|
| `/dev/cpuset/*/cpus` | `CpusetCpus` | cpu mask（`0-3,4-5`） |
| `*/cgroup.procs`、`*/tasks` | `CgroupProcs` | pid 列表 |
| 含 `/cpufreq/` 且 `_freq` 结尾 | `Cpufreq` | kHz 原样 |
| `*/online` | `PerCpu` | 0/1 |
| 其余（devfreq `*_freq`、`msm_performance/*`、`/proc/ppm/*`） | `String` | 原样 |

> 旧表（string/percluster/percpu/uxaffinity 等 6 类的猜测）**作废**：
> 实测没有 `{0}` 占位展开，也没有 uxaffinity 的痕迹。

优化要求（原版核心卖点）：**切换动作时与上一动作 diff，跳过相同值**（同 node 同值只写一次）。

**执行侧现状**：三个 sink，同一个 `Sink` trait。

| sink | 何时用 | 行为 |
|---|---|---|
| `CollectingSink` | 测试 / 无落点 | 只收集 |
| `UnderRootSink` | `UPERF_FAKE_ROOT` 设置时（离线校验、e2e） | 写到 fake root，带账本 |
| `RealSink` | **`modules.sysfs.enable: true`** 且无 fake root | 写真节点：**fd 缓存**（sysfs 的 open 比 write 贵）、**去重**（值等于本 sink 上次写过的就跳过）、**先记账再写**、**写后回读**（被驱动钳制/忽略的计入 `ineffective`，不当成功）|

出厂 63 份配置全是 `enable: false`，所以**默认行为不变**：没有真实写入。`apply_pending` 的决策是
`fake_root` > `enable` > 只打 `would-write`，而 `enable` 由 `Orchestrator::sysfs_enabled()` 从配置读，
sink 由 `Daemon` 持有（一个实例——fd 缓存与去重表就是靠"跨计划复用"才有意义），三个生产者
（dispatcher / preset watcher / 帧源 hint 回调）共用它。

`WriterKind` 的语义由 `SysfsWrite::payload()` 承担：`CgroupProcs` 一行一个 PID，其余单值——因为
`serialize_value` 把数组渲染成 `a,b,c`，那不是 `cgroup.procs` 能接受的形式。真机验证见
`scripts/realsink-device.sh`（knob 表指向 `/data/local/tmp` 的普通文件，代码路径是真的、落点是安全的）。

### 8.4 CPU 调频器（`config/README.md` 六步，逐条实现）

1. 采样：有负载时 `baseSampleTime`、空载时 `baseSlackTime` 周期采样（数据源 `/proc/stat` + `/sys/.../scaling_cur_freq`）。
2. 需求：`demand = load + (1 - load) * (margin + burst)`；集群负载增量 > `predictThd` 时用预测负载并忽略 `latencyTime`。
3. 频点：按 `latencyTime` 分摊升频延迟；低于 `sweetFreq` 的频点无额外延迟。
4. 功耗限制：PL1=`slowLimitPower` / PL2=`fastLimitPower`，池容量 `fastLimitCapacity`、恢复缩放 `fastLimitRecoverScale`；`burst != 0` 时忽略两个限制。
5. 引导调度：`guideCap` 调集群容量；`limitEfficiency` 限制低性能集群频点能效。
6. 写入：走 §8.3 的 cpufreq 写入通道。整体周期耗时目标 ~0.5ms。

能耗模型（`modules.cpu.powerModel[]`，按集群顺序，字段：`efficiency / nr / typicalPower / typicalFreq / sweetFreq / plainFreq / freeFreq`）；
**典型频点不是调频上限**，高于 `typicalFreq` 用外插。

#### 8.4.1 能耗模型已解出（精确闭式）[V]

用上游二进制启动时自己打印的 25 组 `opp <freq> pwr <x> cost <y>` 反推得到
（`docs/records/m5-cpu-governor.md` §1）：

```
x = freq_GHz / typicalFreq;   Rp = plainFreq/typicalFreq;   Rs = sweetFreq/typicalFreq
ratio(x) =  x<Rp: Rs·Rp·x   |   x<Rs: Rs·x²   |   x≥Rs: x³
power = typicalPower · ratio(x)                  [W/核]
cost  = power / ((efficiency/100) · freq_GHz)    [W / 相对GHz]
```

25/25 组上游数值全部复现到 < 0.0015（`uperf-config/src/cpu.rs` 的黄金测试）。
`freeFreq` 不出现在曲线拟合里（README 说它是"最低功耗频点"，是容量下界的语义）。

#### 8.4.2 控频通道（真机实测）[V]

`scaling_max_freq` 在本内核是**驱动级只读**（mode 444；root 也 EACCES，无 AVC 记录），
`scaling_driver = qcom-cpufreq-hw`。上游 8 种 `CpufreqWriter*` 全要 min-freq 节点，全部打不开
→ **原版 uperf v3 在 alioth 上根本控不了频，`No CpufreqWriter supported for this platform` 直接退出**。

可用通道是 `scaling_governor=userspace` + `scaling_setspeed`（实测逐档跟随）。
代价：这是**完全接管**（内核不再自行调频，必须每周期发布目标），且进程若未 disarm 就消失，
policy 会永久停在最后写入的频点。因此 `cpp/uperf/app_main.cpp` 已修：
worker 收到 `TERM_SIG` 先调 `uperf_rs_stop()`（disarm + 还原 governor），
daemon 的 `SIGTERM/SIGINT` 分支改为先给 worker 发信号再退出（原版直接 `exit()`，会留孤儿 worker）。

#### 8.4.3 实现与近似

* 功耗预算分配：受限时给**有负载**的集群取"边际成本（W/相对GHz）低于共同上限"的最高档，
  二分该上限使总有功落在 `limit` 内 —— 边际成本相等即"限定功耗下总容量最大"的 KKT 条件，是我的读法 [I]；
* 功耗归因：`cluster_power_at_khz × 集群负载`（只有忙核耗电）。曾用无条件空载底值，
  叠加 `limitEfficiency` 把空载集群顶在高档后吃掉大半 PL1，把满载 cluster0 压到 403 kHz [V]；
* 升频延迟：每周期最多升/降一档，`predict` 触发时直接跳到目标 —— 复现 README 说的
  "离散采样导致实测延迟总大于 latencyTime"，**不声称与上游逐 tick 一致** [I]；
* OPP 候选集：上游只打印了设备表的一个子集，规则拟合不出来（不是成本去重/凸包/阈值），
  疑为当时抓取残缺 [U]。本实现用**全量**设备 OPP 表（是上游可选集的超集）。

真机验收数据（`config/sdm888.json` / balance / idle，PL1=1.0W）：
空载 c0≈0.88GHz、c1=1574400、c2=1747200（`limitEfficiency` 语义）、pool=15.00；
把 4 个 spinner 用 `taskset 0f` 钉在 cpu0-3 → c0 目标 **1612800 kHz**（0.216W×4=0.864W ≤ 1.0W，
下一档 1708800 是 1.028W > 1.0W，正是 PL1 下最高可行档），c1/c2 空闲不受影响；撤载后回落 883200、池回满。

### 8.5 上下文调度器（`modules.sched`）

* `cpumask`：名字 → CPU id 列表。
* `affinity[类][scene]`：`bg/fg/idle/touch/boost` → cpumask 名（空串 = 不设）。
* `prio[类][scene]`：`0`=跳过，`1~98`=SCHED_FIFO，`100~139`=SCHED_NORMAL，`-1/-2/-3`=NORMAL/BATCH/IDLE。
* `rules[]`：`name / regex / pinned / rules[{k,ac,pc}]`，按数组顺序 = 匹配优先级；`/HOME_PACKAGE/`→启动器包名，`/MAIN_THREAD/`→主线程名（运行时替换）。
* 场景来自 `initials.sched.scene`（`idle|touch|boost`）+ 前台/后台判定（由 `topapp.pkgName` / `cgroup.*` 事件驱动）。
* 正则：**用 Rust `regex` crate，不需要 PCRE2**（原 §3.2 的"PCRE2 必须"是未经验证的假设，已由数据推翻）。
  实测 63 份配置共 2528 处 pattern、**28 条唯一模式**，全为普通 ERE：无 lookaround/lookbehind/atomic/branch-reset/
  conditional/recursion/`\K`/inline flag/反向引用。见 `docs/records/m6-sched-evidence.md` §2 与
  `uperf-config/tests/all_sched_configs.rs::no_shipped_pattern_needs_pcre2`（新增 PCRE-only 语法会编译失败）。
  全量编译自检在 `all_sched_configs.rs`：101 个 sched 模块 / 1014 条进程规则 / 4048 个 pattern。
* `pinned` 语义（README line 248）：**"始终作为处于顶层可见的进程应用规则"**，不是"首匹配优先"。
* `affinity` 取值可以是**逗号分隔的多个 cpumask 组名**（`sdm8g3.json` 用了 `"c1,c2"`，全树 8 处），取并集。
* 已发布配置含**真实缺陷且被上游容忍**（`sdm8g2/8g3` 漏定义 `affinity.fuck`；`sdm7g1` 连 `prio.fuck` 也没有），
  所以悬空类别 = 该维度 no-op，并记入 `Anomaly`；验收闸断言异常集合**恰好等于**白名单三项。

---

## 9. 构建

### 9.1 工具链

* NDK：与 dfps 一致 `aarch64-linux-android23-*`（r24 已验证；本机 r30 亦可，但需重跑 §10.1）。
* Rust：`aarch64-linux-android` target + `cargo-ndk`。
* `build.sh` 任务：`make | pack | install | reboot | clean | check`（沿用 dfps 的 build.sh 风格）。

### 9.2 流程

```bash
# 1) Rust staticlib
cargo ndk -t arm64-v8a -o /dev/null build --release -p uperf-core   # 产出 libuperf_rs.a
# 2) C++ + 链接（CMake，沿用 dfps 的编译/链接选项）
cmake -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=$NDK/.../aarch64-linux-android23-clang \
      -DCMAKE_CXX_COMPILER=.../aarch64-linux-android23-clang++ -H. -Bbuild/arm64 -G "Unix Makefiles"
cmake --build build/arm64 --target uperf -j
# 3) 产物落到 magisk/bin/uperf，并 pack/install
sh build.sh Release make pack install
```

链接选项沿用 dfps：`-ffixed-x18 -Wl,--hash-style=both -fPIE -Wl,-exclude-libs,ALL -Wl,--gc-sections`，
Release 追加 `-static -flto -O3 -Wl,--icf=all,--lto-O3,--strip-all`；C++ 用 `c++_static` + `-fno-rtti -fno-threadsafe-statics`。
C 侧禁止 `-fno-exceptions`（桥要能抛给 upper 层兜底）。

### 9.3 产物断言（`build.sh check`，必须脚本化）

* `file magisk/bin/uperf` → aarch64 PIE
* `readelf -d` → NEEDED 仅 `libm.so / libdl.so / libc.so`
* 已 strip（无 `.symtab`）；RELR/RELRO = full；含 `BIND_NOW`
* 大小 < 3 MB（原版 1.46 MB；超出说明依赖膨胀）
* 冷启动到打印 "uperf is running" < 2s（真机）

---

## 10. 测试与验收

### 10.1 单元测试（`cargo test --release`，主机运行）

当前 288 个（`uperf-core` 141、`uperf-config` 79、`uperf-dfps` 29、`uperf-cli`/集成 39）。覆盖的是
**能在主机上判定**的东西：

* 配置：63 份 `config/*.json` 全部解析；`template.json` 的空值是合法输入；层叠覆盖
  （`initials` → `presets.*` → `presets.<mode>.<scene>`）；38 份上游基线配置在 `uperf-cli` 的集成测试里。
* 规则引擎：所有 `sched.rules[].regex` 与 `k` 编译通过，`/HOME_PACKAGE/`、`/MAIN_THREAD/` 替换后语义正确；
  新增 PCRE-only 语法会让 `all_sched_configs` **编译失败**（§8.5）。
* 调频数学：`demand` / `predictThd` / 功耗池 / `burst` 纯函数 + 表驱动；能耗模型对上游打印值 <0.0015。
* 机制层：hint 状态机（§8.1）、帧源阶梯（`frame_state`）、`SysfsWrite::payload()` 逐 kind、
  `RealSink`（落盘/去重/账本/写后回读）、ctl 协议与 `STATUS` 分帧、`procfs`、`Settings` 解析、
  任务注册表与信号路径。

### 10.2 parity 工具（离线，不需要设备）

> 两套配置要分清：**上游 v3 发行包** 38 份在 `docs/configs/upstream/`（parser 回归基线）；
> **本仓库（UGT 基线）** 63 份在 `config/`，那才是发货目标。

```bash
uperf-cli parse  <config.json>                  # 结构摘要（meta/modules/initials/presets）
uperf-cli warn   <config.json>                  # 告警行（与上游 CfgMgr 日志逐字一致）
uperf-cli plan   <config.json> <mode> <scene>   # 层叠结果 + 每个键的来源
```

判定：63 份 × 5 preset × 7 scene 全组合的 `warn`/`plan` 与参考输出差异为 0，或每处差异都有书面理由。
`plan` 额外标出来源（`preset[scene]` / `preset[*]` / `init`）；原计划的 `expand` 子命令已并入 `plan`。

### 10.3 设备验证脚本（真机验收的实际入口）

主机跑得动的：`scripts/test_watchdog_host.sh`（看门狗，109 断言，含每个脚本 `sh -n`）。

需要 alioth 的：

| 脚本 | 验什么 |
|---|---|
| `scripts/cpufreq-write-probe.sh` | 内核 knob 可写性（mode 位 vs 实际写入） |
| `scripts/p1-signal-device.sh`、`p1-signal-burst-device.sh` | 停机路径：8 轮 `killall uperf` + 20 连发 SIGTERM，断言退出、disarm、`state=stopped` |
| `scripts/p2-task-registry-{1task,4tasks}.sh` | 任务注册表与停止顺序（governor 先停，其余逆序） |
| `scripts/takeover-device.sh` | 真实 sysfs 的 `userspace` 接管与恢复 |
| `scripts/realsink-device.sh` | `RealSink`（knob 表指向 `/data/local/tmp` 的普通文件） |
| `scripts/install-and-verify-staged.sh`、`post-boot-verify.sh`、`post-boot-restart-verify.sh` | 装机、开机后文件完整性、两次热重启 |
| `tools/uprobe-probe/e2e-*.sh`、`tools/binder-probe/e2e-*.sh`、`tools/ctl-listen/e2e-ctl.sh` | 两条帧源腿、控制面 `STATUS` |

纪律：sysfs 默认走 `UPERF_FAKE_ROOT`（`RealSink` 例外——它按定义写真路径，所以测试时把 knob 指到
`/data/local/tmp`）；调度必须同时 `UPERF_SCHED_DRY_RUN=1`；**每次**重启后核对模块文件 md5 与全 NUL
扫描（§2 的内核缺陷）。`media/*.png` 是上游行为基线截图，仅供对照，不构成门槛。

### 10.4 完成判定（硬性）

* "已实现"必须附带：`cargo test` 输出、`uperf-cli plan` 的 diff、或真机日志/trace 片段。
* 禁止：把 UNKNOWN 写成 VERIFIED；用"应该/预期"代替实测；没跑 parity 就宣布配置兼容。
* 没做完的写进 §12，不静默略过。

---

## 11. 功能清单（①..⑪ 就是代码里那些圆圈编号）

M11 当初排的队列，编号已经成了代码注释里的词汇（`⑪` = uprobe 帧计数等），所以留在这里；**逐里程碑的
证据、真机结论与"当时没做什么"在 [`docs/history/milestone-log.md`](./docs/history/milestone-log.md)**。

| # | 功能 | 默认 | 代码 |
|---|---|---|---|
| ① | 看门狗稳态成本缓存化（活体判定走 pid+start_ticks 缓存，过期才全扫 `/proc`） | 开 | `magisk/script/uperf_watchdog.sh` |
| ② | 写入账本 + 恢复断言（改过的 sysfs knob 先记原值，停止/被杀后由脚本回滚） | 开（有写入才记账） | `src/sysfs_ledger.rs`、`libuperf.sh::uperf_restore_sysfs` |
| ③ | SF 重启后自动重注入 | `UPERF_SF_INJECT=1` | `uperf_watchdog.sh` |
| ④ | 前台助手文件源（`foreground.txt` 作 top-app 来源，C++ monitor 兜底） | 开（文件不存在即回落） | `src/foreground.rs` |
| ⑤ | 直连 binder 的 SF `--latency` 帧源（降级腿） | `UPERF_SF_BINDER=1` | `src/sf_binder.rs` |
| ⑥ | 热反馈削 PL1（§8.4 的 thermal） | 随 governor（默认关） | `uperf-config/src/thermal.rs` |
| ⑦ | socket 控制面：反连握手 + `PING`/`PONG`/`STATUS` | `UPERF_CTL_SOCKET` | `src/ctl_socket.rs` |
| ⑧ | 记录通道（按 pkg+epoch 的会话，≥3 min，DEFLATE） | `UPERF_RECORD=1` | `src/recorder.rs` |
| ⑨ | 配置标准档合并 | **放弃**（会动到与上游逐字对齐的配置格式） | — |
| ⑩ | 32 位覆盖的诚实声明 + 装机守卫（32 位构建明确不做） | 守卫常开 | `libsysinfo.sh::require_aarch64` |
| ⑪ | uprobe 每线程帧计数（AppOpt 风格的主腿，perf PMU，不加载 BPF） | `UPERF_SF_UPROBE=1` | `src/sf_uprobe.rs`、`src/frame_state.rs` |

## 12. 仍未解决的问题

只有**当前**仍然成立、且会影响判断的几条。已解决的（以及当时怎么坏的）在
[`docs/history/unknowns-resolved.md`](./docs/history/unknowns-resolved.md)。

### 12.1 SfAnalysis 的 hook 点（M8 已重写，0 闭源 blob）

结论：在 alioth 上 `execmem`/`execmod` 都被拒（SELinux + 无可用 sepolicy 通道），所以 hook 由
**GOT/PLT 改写**安装（只需 `mprotect` 数据页），这也是 vendor 的静态 xHook 本来就在做的事。
注入库 `libsfanalysis_rs.so` 由本仓库构建、独立 profile（`panic = "unwind"`，见 §9），
默认只装库不碰 SF，`UPERF_SF_INJECT=1` 才注入。规格与逆向证据在
[`docs/spec/sfanalysis.md`](./docs/spec/sfanalysis.md)、
[`docs/records/m8-sfanalysis-reverse.md`](./docs/records/m8-sfanalysis-reverse.md)；当年怎么排查的在
[`docs/history/unknowns-resolved.md`](./docs/history/unknowns-resolved.md) 与 §12.1 原文。

### 12.2 其它未知

| 项 | 状态 | 影响 / 做法 |
|---|---|---|
| 上游打印的 OPP 列表是设备表的子集（cluster0 打了 9/17），规则拟合不出（非成本去重/非凸包/非功率阈值） | **UNKNOWN**（疑为抓取残缺） | 本实现用**全量**设备 OPP 表（上游可选集的超集），可达频点范围只多不少 |
| 上游的功率预算分配规则、latency 平滑细节 | **UNKNOWN** | 本实现是"每样本一个 OPP、`predict` 触发才跳"，不是渐近逼近 |
| `modules.input.enable=false` 是否真的能停掉输入监听 | **UNKNOWN** | 63 份配置全 `true`，且 vendored 监听器在配置解析前就由平台层启动；配成 false 只会多打一行日志 |
| `cur_powermode.txt` 里 `auto` 的语义 | **[I]** 读作"交给 perapp 规则"（二进制里只有 `Internal perapp switcher …` 的痕迹） | 代码按 `[I]` 实现，未真机对照过原版 |
| `comm` 被内核截断到 15 字符 | **已知**（不是缺陷） | `/MAIN_THREAD/` 用观测到的 `comm` 替换，模式与线程名同源 |
| 帧源两条腿只在人工 e2e 里验过（`tools/uprobe-probe/e2e-shade.sh`、`tools/binder-probe/e2e-frames.sh`） | **已知** | 默认都不开；开机后 `frame.source=` 为空是正常的 |

### 12.3 许可与署名

* 本仓库内容与上游一致为 **Apache-2.0**；vendor 的 dfps 代码保留其 "Copyright (C) 2021-2022 Matt Yang" 头。
* `NOTICE` 需追加：dfps 来源与 commit、Rust 依赖清单。
* 仓库内保留的原版二进制仅作 parity 参照，发布产物必须替换为自研构建。

## 13. 编码规范

* Rust 2021；`#![deny(unsafe_op_in_unsafe_fn)]`；`unsafe` 只允许出现在 `ffi.rs` 与 syscall 包装层，每处必须有 `// SAFETY:` 说明。
* 运行路径禁止 `unwrap/expect/panic`（`panic = "abort"` 也救不回被 abort 的整机性能控制器）。
* 所有跨线程共享状态用 `Mutex`/`Arc`；禁止 `static mut`。
* 环境变量只在 `settings.rs` 读一次；模块内出现 `std::env::var` 视为回归。
* C++ 侧改动最小化，且改动必须记入 `cpp/dfps/DFPS_VENDOR.md`。
* 提交信息英文、conventional（`feat(governor): ...`），按类别聚合；不提交构建产物（`/build` 已在 `.gitignore`，仍要确保 `magisk/bin/uperf` 的替换是有意为之）。

---

## 14. 快速命令

```bash
# 同步上游（只动 master，不动 game-turbo 的业务改动）
git fetch upstream && git checkout master && git merge --ff-only upstream/master

# 构建 / 打包 / 装机
sh build.sh Release make pack install

# 离线 parity
cargo run -p uperf-cli -- plan config/sdm855.json balance touch | head -40
cargo run -p uperf-cli -- warn config/sdm855.json

# 真机
adb -s <serial> shell su -c 'magisk --install-module /data/local/tmp/uperf-magisk.zip'
adb -s <serial> shell 'cat /sdcard/Android/yc/uperf/uperf_log.txt'
```
