# AGENT.md — Uperf 重写方案（Rust 核心 + 复用 dfps C++ 平台）

> 本文件是本仓库的**施工说明与验收标准**。任何在本仓库工作的 agent 必须先读完本文件，
> 并在提交时对照第 10 节的验收条目自查。**禁止把"应该能"写成"已验证"。**

- 仓库：`grill-glitch/uperf-rewrite`（fork，网络根 `yc9559/uperf`）
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
- 不重写 SfAnalysis 的注入库（`libsfanalysis.so` 仍用厂商件；见 §12.1）。
- 不追求与闭源二进制逐指令等价；追求**配置语义等价 + 运行时行为等价**（用 §10 的 parity 工具证明）。

**成功判据（全部要有真机证据）**：
1. `magisk/bin/uperf` 被自研二进制替换后，模块在 alioth（crDroid A16 / KernelSU Next）与 polaris（LineageOS 22.2）上开机自启成功，`/sdcard/Android/yc/uperf/uperf_log.txt` 无 `[E]`，`killall uperf` 能正常停止。
2. 63 份 `config/*.json` 全部解析成功，且**告警/忽略行与原版逐字一致**（见 §10.2）。
3. 在 §10.3 的脚本化 sysfs 镜像上，切换全部 5 个 preset × 7 个 scene，**写入序列与原版一致**。
4. §10.4 的 7 张 golden trace 场景中，hint 状态机的跳转顺序与原版一致。

---

## 2. 现状基线（VERIFIED 事实，勿再猜测）

| 事实 | 证据 |
|---|---|
| `magisk/bin/uperf` = 上游 `dev-22.09.04` 二进制 | sha256 `f1265757009ff0c85dd8587d9e7bfcf5e51d10d36fe5e1341688215ae1fb49d8`（与上游发行包一致） |
| 该二进制 1,461,512 B、aarch64 PIE、stripped、NDK r24、`.text` 987,320 B / 2,320 函数 | `readelf` / `.eh_frame_hdr` 二分表解析（自洽校验通过） |
| 只依赖 `libm/libdl/libc`，`BIND_NOW` + full RELRO | `readelf -d/-l` |
| 26 个自有类名（含 `CpufreqWriter` 全家族）、14 个源文件路径、全量日志串保留在二进制里 | RTTI 名 + `__FILE__` 串 + `strings` |
| 调用契约：`bin/uperf <USER_PATH>/uperf.json -o <USER_PATH>/uperf_log.txt`，`USER_PATH=/sdcard/Android/yc/uperf` | `magisk/script/libuperf.sh:38`；`Usage: uperf [-o log_file] config_file` 亦在二进制内 |
| 进程名必须是 `uperf`（`killall uperf` 停服）；启动后脚本会把它移入 background cpuset | `libuperf.sh: uperf_stop/uperf_start` |
| 配置由 `setup.sh` 从 `config/<soc>.json` 拷成 `USER_PATH/uperf.json` | `magisk/script/setup.sh:67` |
| 配置 schema v3 = `meta` / `modules` / `initials` / `presets`，**层叠覆盖用点号键**（`cpu.margin`、`sysfs.xxx`、`sched.scene`） | 仓库内 `config/README.md`（376 行，v3 权威规范） |
| Q: 根 `README.md` 里的 `powermodes/actions`、`knob type` 是 **v1/v2 旧文档**，与 v3 配置不符 | 对照 `config/template.json` 与 `config/sdm855.json` 实际键 |
| 事件总线 topic 与 dfps 逐字相同 | `cgroup.{ta,fg,bg,re}.{list,update}`、`input.{touch,btn,state}`、`offscreen.state`、`topapp.pkgName`；uperf 独有 `anim.running`、`sfanalysis.hint`、`config.profile` |
| uperf 比 dfps 多出的能力层 | `context_scheduler`（PCRE2 正则规则引擎）、`cpu_governor` + `cpu_busy_reader`、`profile_switcher`、`sysfs_writer` + 12 个 `CpufreqWriter*`、`sfanalysis_listener`、`anim_watcher`、`log_level_switcher` |

---

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
│  app      模块装配（替代 uperf.cpp）                            │
│  config   JSON 解析 + 层叠覆盖 + 兼容性告警                     │
│  switcher hint 状态机 + preset/perapp 切换 + 时长                │
│  profile  preset/scene → 各模块参数表下发                       │
│  sysfs    6 类写入器（string/percluster/percpu/cpufreq/         │
│           cgroup_procs/uxaffinity）+ 去重 + fd 缓存             │
│  governor 负载采样 + 能耗模型 + 功耗池(PL1/PL2) + 频点决策       │
│  sched    上下文调度规则引擎（PCRE2 语义的正则匹配）             │
│  sf       sfanalysis.hint 消费 + 渲染结束/Hint 提前结束          │
│  anim     anim.running 消费（系统动画）                          │
│  log      atrace/日志级别切换                                    │
└──────────────────────────────────────────────────────────────┘
```

**决策（已定，勿再反复）**：**C++ 拥有 `main()` / 进程骨架 / 日志 / 事件总线；Rust 以 staticlib 被链接，承担全部被重写的业务模块。**
理由：`main.cpp` 的 fork+SIGCHLD+tombstone+SIGUSR1 监督器是最容易写错的一块，dfps 已验证可用；把它留在 C++ 既最大化复用，又让"重写部分 = Rust"的边界清晰。

### 3.2 Rust 依赖（保持最小）

| crate | 用途 | 备注 |
|---|---|---|
| `serde` + `serde_json` | 配置解析 | 不允许换成手写 parser |
| `pcre2`（绑定 PCRE2） | `sched` 规则正则 | **必须**保留 PCRE2 语义：现有配置含 `^(RenderThread\|GLThread)`、`/HOME_PACKAGE/`、`/MAIN_THREAD/`、可能的 POSIX 类 `[[:<:]]`；Rust `regex` crate 不支持 POSIX word-boundary，若改用 `regex` 必须在 §10.2 里证明 63 份配置全部规则语义一致 |
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
  Cargo.toml                workspace
  uperf-core/               staticlib crate（pub extern "C" 入口）
    src/{lib.rs,ffi.rs,config/,switcher/,profile/,sysfs/,governor/,sched/,sf/,anim/,logmod/}
  uperf-cli/                纯 Rust 工具（离线配置/parity 用，不随模块发布）
/docs/
  spec/config-v3.md         从 config/README.md 提炼的机器可校验规范
  spec/sfanalysis.md        SF 侧现状与未知项（§12.1）
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
| `sysfs` 写入器（6 类） | **Rust** | `sysfs/`，见 §8.3 |
| `cpu` 调频器（governor + 负载采样） | **Rust** | `governor/`，见 §8.4 |
| `sched` 上下文调度器 | **Rust** | `sched/`，见 §8.5 |
| `sfanalysis` 监听 | **Rust** | `sf/`，见 §7.4 / §12.1 |
| `anim_watcher` / `log_level_switcher` / `atrace_switcher` | **Rust** | `anim/`、`logmod/` |
| `CpufreqWriter{Msm,Ppm,Epic}{Base,Fixed,Powersave}` + `Performance` | **Rust** | 平台差异（`/sys/kernel/msm_performance/...`、`/proc/ppm/policy/...`、`/dev/cluster{}_freq_{min,max}`）在 Rust 里用策略表表达 |
| `libsfanalysis.so`（SF 注入库）/ SsAnalysis | **不动** | 仍用厂商件，见 §12 |

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

### 7.3 日志格式

原版（README 示例）：`[13:03:33][I] CfgMgr: Using [sdm855/sdm855+ v20200516] by [yc@coolapk]`
**Rust 侧日志必须产出同样的可正则解析格式**：`[HH:MM:SS][L] <Tag>: <msg>`。
必须保留的关键行（parity 断言的锚点）：
```
CfgMgr: Using [<name>] by [<author>]
CfgMgr: Read default powermode from <switchInode>
CfgMgr: Powermode "<old>" -> "<new>"
CfgMgr: Bind HintNone -> <action>            （v3 的 hint 命名不同，以 v3 行为为准，需实测对齐）
CfgMgr: Ignored root/platform/knobs/<knob> [Disabled by config file]
CfgMgr: Ignored root/platform/knobs/<knob> [Path is not writable]
CfgMgr: Ignored knobs in action ...: <names>
SfAnalysis: Surfaceflinger analysis connected
```
> ⚠️ v3 的 `hint` 命名与 v2 README 的 `HintNone/HintTap/...` **可能不同**（v3 引入了 `idle/touch/trigger/gesture/junk/switch`）。**以真机实测原版日志为准**，不许照抄 v2 文档。

### 7.4 `sfanalysis.hint` 文件协议

SF 侧注入库向外部写单字节状态（原库实现：`lseek+read/write` 单字节状态机，状态码 0..3）。
Rust 侧必须：
1. 按 `modules.sfanalysis.enable` 与模块 flag 决定是否启用；
2. 监视 hint 文件（inotify）；
3. 把状态映射到「渲染开始 / 渲染滞后 / 渲染结束」，并按 `renderIdleSlackTime` 判定渲染结束；
4. 在渲染结束信号后 66ms 内结束 Hint（README 所述行为基线）。
**状态码到语义的映射未知（§12.2）**：M2 阶段必须先真机抓取原版行为（Frida hook 或抓日志）再实现，禁止猜。

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

### 8.2 配置层叠（`CSS 式覆盖`）

优先级：`presets.<mode>.<scene>` > `presets.<mode>.*` > `initials.<module>.<param>`；
键名带模块前缀（`cpu.baseSampleTime`、`sysfs.cpusetTa`、`sched.scene`）。
`modules.*` 为静态段，只在实例化时读一次。
**必须复刻原版的告警语义**：定义了在 `modules` 里不存在的模块/键 → 按原版文案告警并忽略（§10.2 逐条对账）。

### 8.3 sysfs 写入器（6 类）

| 类型 | 语义（来自 v2 文档 + v3 行为，需实测校正） |
|---|---|
| `string` | 直写 `echo val > path` |
| `percluster` | 用 `clusterCpuId` 替换 `path` 里的 `%d`，值逗号分隔 |
| `percpu` | 按 `efficiency` 列表长度生成核心序列，值逗号分隔 |
| `cpufreq` | `percluster` 变体，值 = 设定值 × 100000，写入失败要重试（处理 new min > old max） |
| `cgroup_procs` | 进程名 → PID 替换，最多 4 值，**关闭去重**（线程会变） |
| `uxaffinity` | 1 = 把顶层 APP 的 UI 相关线程绑到大核；0 = 放开全部核心；顶层 APP 变化时重扫 |

优化要求（原版核心卖点）：**切换动作时与上一动作 diff，跳过相同值**；节点以 fd 缓存常开；单次切换开销要能在真机上用 atrace 量到（原版量级：轮询 0.4ms/100ms）。

### 8.4 CPU 调频器（`config/README.md` 六步，逐条实现）

1. 采样：有负载时 `baseSampleTime`、空载时 `baseSlackTime` 周期采样（数据源 `/proc/stat` + `/sys/.../scaling_cur_freq`）。
2. 需求：`demand = load + (1 - load) * (margin + burst)`；集群负载增量 > `predictThd` 时用预测负载并忽略 `latencyTime`。
3. 频点：按 `latencyTime` 分摊升频延迟；低于 `sweetFreq` 的频点无额外延迟。
4. 功耗限制：PL1=`slowLimitPower` / PL2=`fastLimitPower`，池容量 `fastLimitCapacity`、恢复缩放 `fastLimitRecoverScale`；`burst != 0` 时忽略两个限制。
5. 引导调度：`guideCap` 调集群容量；`limitEfficiency` 限制低性能集群频点能效。
6. 写入：走 §8.3 的 cpufreq 写入通道。整体周期耗时目标 ~0.5ms。

能耗模型（`modules.cpu.powerModel[]`，按集群顺序，字段：`efficiency / nr / typicalPower / typicalFreq / sweetFreq / plainFreq / freeFreq`）；
**典型频点不是调频上限**，高于 `typicalFreq` 用外插。

### 8.5 上下文调度器（`modules.sched`）

* `cpumask`：名字 → CPU id 列表。
* `affinity[类][scene]`：`bg/fg/idle/touch/boost` → cpumask 名（空串 = 不设）。
* `prio[类][scene]`：`0`=跳过，`1~98`=SCHED_FIFO，`100~139`=SCHED_NORMAL，`-1/-2/-3`=NORMAL/BATCH/IDLE。
* `rules[]`：`name / regex / pinned / rules[{k,ac,pc}]`，按数组顺序 = 匹配优先级；`/HOME_PACKAGE/`→启动器包名，`/MAIN_THREAD/`→主线程名（运行时替换）。
* 场景来自 `initials.sched.scene`（`idle|touch|boost`）+ 前台/后台判定（由 `topapp.pkgName` / `cgroup.*` 事件驱动）。
* 正则：用 PCRE2（见 §3.2），并在 §10.2 里对 63 份配置的**全部** regex 做一次编译+匹配自检。

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

### 10.1 单元测试（`cargo test`，主机运行）

* 配置：63 份 `config/*.json` 全部解析；`config/template.json` 的空值是合法输入；层叠覆盖断言（`initials` → `presets.*` → `presets.<mode>.<scene>`）。
* 规则引擎：遍历所有配置里的 `sched.rules[].regex` 与 `k`，断言可用 PCRE2 编译，且 `/HOME_PACKAGE/`、`/MAIN_THREAD/` 替换后语义正确。
* 调频数学：`demand` 公式、`predictThd` 分支、功耗池增减/恢复、`burst` 旁路 —— 纯函数 + 表驱动测试。
* 状态机：按 §8.1 逐条转移，含超时与"渲染结束提前退出"。
* sysfs 值展开：6 类写入器的字符串展开（`percluster`/`percpu`/`cpufreq`×100000/`cgroup_procs`/`uxaffinity`），用黄金用例。

### 10.2 parity 工具（关键：必须有真证据）

`rust/uperf-cli` 提供两个子命令，**离线**对账（不需要设备）：

```
uperf-cli parse  <config.json>              # 输出解析后的展开表（JSON）
uperf-cli expand <config.json> <mode> <scene>   # 输出该场景下所有 knob 的最终值
uperf-cli plan   <config.json> <mode> <scene>   # 输出 sysfs 写入序列（path=value）
uperf-cli warn   <config.json>              # 输出告警行（应与原版日志逐字一致）
```
验收：对 63 份配置 × 5 preset × 7 scene 全组合跑 `warn`/`plan` 并与原版日志/实测写入比对，
差异必须为 0（或每一处差异都有书面理由）。

### 10.3 真机 parity（sysfs 镜像法）

1. 用 `jq` 把 `modules.sysfs.knob` 的所有 path 重写到一个 tmpfs 镜像目录（`/data/local/tmp/fakesys/...`），生成 `uperf.fake.json`。
2. 原版二进制与自研二进制各跑一遍，同样的模式/场景序列（脚本写入 `cur_powermode.txt` + 模拟 input 事件）。
3. `diff` 两次的写入序列与最终文件内容 —— **必须一致**。
4. 真机再跑一轮**未重写路径**的对照：真实 sysfs 上比较 `atrace`/`perfetto` 上的 CPU 频点曲线。

### 10.4 行为基线（仓库内 7 张 golden trace）

`media/*.png` 是原版在真机上的 Perfetto 行为基线，逐张当作验收场景：
`wechat_resume`（热启动重负载）、`android_am`（顶层 APP 切换/亮屏）、`sflag`（渲染滞后即刻升频）、
`render_stop`（渲染结束 66ms 内退升频）、`render_restart`（滞后 UI 重启渲染则恢复 Hint）、
`fingerprint`（指纹最大性能）、`adjusted_demand_capacity_relation`（需求-容量模型）。
每个场景要有一条可复现脚本 + 一张新 trace，与原图同口径对比。

### 10.5 完成判定（硬性）

* 任何"已实现"必须附带：`cargo test` 输出、`uperf-cli plan` 的 diff 输出、或真机 trace/日志片段。
* 禁止：把 UNKNOWN 写成 VERIFIED；用"应该/预期"代替实测；未跑 parity 就宣布配置兼容。
* 未完成的项写入 §12 的未知清单，不许静默略过。

---

## 11. 里程碑

| 阶段 | 内容 | 交付 | 验收 |
|---|---|---|---|
| **M0** | vendor dfps 到 `cpp/dfps/`，跑通 dfps 原样构建；确定 CLI/日志/进程名适配点 | `DFPS_VENDOR.md`、可编译的 `cpp/uperf` | 在 alioth 上启动自编译的 dfps，日志正常 |
| **M1** | Rust staticlib 骨架 + C ABI 桥跑通：C++ 启动 → 调 `uperf_rs_start` → 订阅 `input.touch`/`topapp.pkgName` → 收到事件打日志 | 骨架 + 桥 | 真机日志出现 Rust 侧收到的事件 |
| **M2** | 配置系统 + `uperf-cli parse/warn`；63 份配置全部解析 | parity 工具 | §10.2 告警对账 0 差异 |
| **M3** | switcher/profile/sysfs（6 类写入器）+ 状态机 | `plan` 输出 | §10.3 假 sysfs 写入序列 0 差异 |
| **M4** | CPU 调频器 + 上下文调度器 | 真实 sysfs 写入 | §10.4 的 `wechat_resume`/`android_am` 场景行为对齐 |
| **M5** | sfanalysis 监听 + anim/log/atrace；整机替换 `magisk/bin/uperf` | 可发布的 Magisk zip | §10.4 全部 7 张基线通过；§1 成功判据全绿 |

M0–M2 之间不得并行改动 `cpp/dfps/**`；M3 起 Rust 侧可并行（config/sysfs/governor/sched 互相独立）。

---

## 12. 风险与未知（当前必须承认的）

### 12.1 SfAnalysis 的 hook 点（**最大未知**）
* 现状：`libsfanalysis.so`（26 KB / ~46 函数，纯 C，静态链 xHook）通过 `patchelf --add-needed` 注入 surfaceflinger；
  它解析 `/proc/self/maps` 定位目标库，再按名挂钩。
* **v3 二进制里已无明文 hook 符号名**（v1 有 `_ZN7android5Fence11waitForeverEPKc`），
  且 `.data` 中有 4 条 13/21/29/29 字节的不透明记录（均以 `0x9e3772XX` 开头、`0xb5` 结尾），静态解不出。
* 本项目决策：**不重写注入库，继续使用厂商 `libsfanalysis.so`**；Rust 侧只负责消费 hint 文件（§7.4）。
  若将来要替换，先按 §10.5 的规则补一份 `docs/spec/sfanalysis.md` 并给出真机 Frida 证据。
* 状态码→语义映射未知：M2 必须真机抓取。

### 12.2 其它未知
| 项 | 状态 | 处理 |
|---|---|---|
| `sfanalysis.hint` 的状态码语义 | UNKNOWN | 真机抓取（M2） |
| v3 的 Hint 命名与日志文案（v2 文档不可信） | UNKNOWN | 以原版真机日志为准（M2 起逐条采集） |
| `.data` 4 条不透明记录 | UNKNOWN | 不影响本项目（不重写该库） |
| 原版 `CpufreqWriter` 各平台子类的确切分支条件 | 部分未知 | 用 63 份配置反推 + 真机写入对照 |
| `context_scheduler` 的原版默认规则效果 | 已文档化但未实测 | §10.4 场景对齐 |
| UGT 的 `asoulopt.zip` / `miui_migt.sh` / `platform_special.sh` / MTK 功耗表 | 不属本项目 | 原样保留，不解析 |

### 12.3 许可与署名
* 本仓库内容与上游一致为 **Apache-2.0**；vendor 的 dfps 代码保留其 "Copyright (C) 2021-2022 Matt Yang" 头。
* `NOTICE` 需追加：dfps 来源与 commit、Rust 依赖清单。
* 仓库内保留的原版二进制仅作 parity 参照，发布产物必须替换为自研构建。

---

## 13. 编码规范

* Rust 2021；`#![deny(unsafe_op_in_unsafe_fn)]`；`unsafe` 只允许出现在 `ffi.rs` 与 syscall 包装层，每处必须有 `// SAFETY:` 说明。
* 运行路径禁止 `unwrap/expect/panic`（`panic = "abort"` 也救不回被 abort 的整机性能控制器）。
* 所有跨线程共享状态用 `Mutex`/`Arc`；禁止 `static mut`。
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
