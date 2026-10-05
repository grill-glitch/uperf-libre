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
| uperf v3 的日志 pattern 就是 dfps 的 `%H:%M:%S %L %v`；`Usage:`/`Uperf is running`/守护横幅/`v3(22.09.04)`/作者串与二进制逐字一致 | 见 §7.3 的证据链 |
| 12 个业务模块源文件名在二进制里保真：`anim_watcher / atrace_switcher / cgroup_listener / context_scheduler / cpu_governor / input_listener / offscreen_monitor / profile_switcher / sfanalysis_listener / sysfs_writer / topapp_monitor / main` | `__FILE__` 串 |
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

### 8.2 配置层叠（**已按 38 份真实配置修正：不是 `base_hint` 嵌套**）

实测的 v3 结构（`docs/upstream-configs/*.json` 全量验证，见 `docs/m2-evidence.md`）：

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
所以 `uperf-cli plan` 与设备侧都从这张表查路径（见 `docs/m4-evidence.md` §1）。

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
（`docs/m5-cpu-governor.md` §1）：

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
  conditional/recursion/`\K`/inline flag/反向引用。见 `docs/m6-sched-evidence.md` §2 与
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

### 10.1 单元测试（`cargo test`，主机运行）

* 配置：63 份 `config/*.json` 全部解析；`config/template.json` 的空值是合法输入；层叠覆盖断言（`initials` → `presets.*` → `presets.<mode>.<scene>`）。
* 规则引擎：遍历所有配置里的 `sched.rules[].regex` 与 `k`，断言可用 PCRE2 编译，且 `/HOME_PACKAGE/`、`/MAIN_THREAD/` 替换后语义正确。
* 调频数学：`demand` 公式、`predictThd` 分支、功耗池增减/恢复、`burst` 旁路 —— 纯函数 + 表驱动测试。
* 状态机：按 §8.1 逐条转移，含超时与"渲染结束提前退出"。
* sysfs 值展开：6 类写入器的字符串展开（`percluster`/`percpu`/`cpufreq`×100000/`cgroup_procs`/`uxaffinity`），用黄金用例。

### 10.2 parity 工具（关键：必须有真证据）

> **两套配置要分清**：
> * **上游 v3 发行包**里是 **38** 份 `config/*.json` → 已 vendor 到 `docs/upstream-configs/`，
>   用作 parser 的回归基线（38/38 通过，见 `docs/m2-evidence.md` §2）。
> * **本仓库（UGT 基线）**里是 **63** 份 `config/*.json`（UGT 在 38 份基础上补了自己的平台）
>   → M3 起用 `uperf-cli` 对**这 63 份**做全量 plan 对账，那才是发货目标。

`rust/uperf-cli` 提供三个子命令，**离线**对账（不需要设备）：

```
uperf-cli parse  <config.json>                  # 结构摘要（meta/modules/initials/presets）
uperf-cli warn   <config.json>                  # 告警行（应与原版 CfgMgr 日志逐字一致）
uperf-cli plan   <config.json> <mode> <scene>   # 层叠后的键值 + 来源（M3 起再接 sysfs path 展开）
```
> 原计划的 `expand` 子命令已并入 `plan`（同一份层叠结果，`plan` 额外标出每个键的来源：
> `preset[scene]` / `preset[*]` / `init`）。
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
| **M0** ✅ | vendor dfps 到 `cpp/dfps/`，跑通 dfps 原样构建；确定 CLI/日志/进程名适配点 | `DFPS_VENDOR.md`、可编译的 `cpp/uperf`、`docs/m0-evidence.md` | **已达成**（2026-10-02，alioth）：`build.sh check` 全绿，进程监督器/日志格式/CLI/4 个事件源/配置热重载全部真机验证；`offscreen.state` 未触发，列入 §12.2 |
| **M1** ✅ | Rust staticlib 骨架 + C ABI 桥跑通 | `rust/uperf-core/`（Cargo.toml + lib.rs + ffi.rs + topic_dispatch.rs + tests/）、`cpp/uperf/bridge.cpp`、`cpp/include/uperf_rs_bridge.h` | **已达成**（2026-10-05，alioth）：`build.sh check` 全绿；10/10 payload 解码单元测试通过；**真机过**：C++/Rust 同步出现 `EventTap:` / `[Rust] Rust: ...` 两份日志，pid list 预览(8/8)字节相等；电源键触发 `offscreen.state=true`，**officially§12.2 第 1 条已解** |
| **M2** 🚧 | 配置系统 + `uperf-cli parse/warn/plan`；38 份配置全部解析 | `rust/uperf-cli/`、`docs/upstream-configs/`、`docs/m2-evidence.md` | **部分达成**：38/38 配置解析通过、`warn` 零误报；`plan` 已出层叠结果。待做：`plan` 的 sysfs 路径展开（需 M3 writer）、与原版日志文案逐条对账 |
| **M3** 🚧 | hint FSM + sysfs writer dispatch | `rust/uperf-core/src/hint.rs`、`rust/uperf-core/src/sysfs.rs`、`docs/m3-evidence.md` | **已达成（构建+fd 验证）**：`SfHint` 枚举（0..5）匹配上游 binary；dispatch 表覆盖 13/14 个真实 device fd；`uperf-cli plan` 与上游 v3 在 alioth 上的 sysfs 写入路径一一对应；待做：把 hint FSM 接入 dispatch loop（事件→hint transition→`plan_scene`→真写）+ UFSmax 这类 SoC 专属 hex 路径发现 |
| **M4** ✅ | 事件→hint→config→sysfs 写入链路 | `rust/uperf-config/`、`rust/uperf-core/src/{hint,orchestrator}.rs`、`docs/m4-evidence.md` | **已达成**（真机）：16 条 sysfs 写入计划与配置路径逐字一致，fake-root 16 文件落地 |
| **M5a** ✅ | CPU 调频器：能耗模型 + 负载采样 + 功耗限制 + 真机控频 | `rust/uperf-config/src/{cpu,governor,gov_build,freq_target,proc_stat}.rs`、`rust/uperf-core/src/cpu_task.rs`、`docs/m5-cpu-governor.md` | **已达成**（alioth）：25/25 上游 `pwr/cost` 黄金值复现；真机 `userspace`+`setspeed` 控频生效；PL1=1.0W 下满载小核贴上限 1612800 kHz；撤载回落；SIGTERM 干净 disarm |
| **M6a** ✅ | 上下文调度器（`modules.sched`）：配置层 + 内核应用 + 真机验证 | `uperf-config/src/sched.rs`、`uperf-core/src/{sched_apply,sched_task}.rs`、`uperf-config/tests/all_sched_configs.rs`、`docs/m6-sched-evidence.md` | **已达成**：全配置闸通过（101 模块/1014 规则/4048 pattern，异常集恰为白名单 3 项）；真机隔离实验 `0-7/policy5 → 0-1/policy3` 双值命中且 6s 稳定；写后回读校验；`SIGCHLD` 跨层 bug 已修 |
| **M6b** ✅ | 预设热切换（`cur_powermode.txt` + perapp 规则）、`sfanalysis.hint` 监听、`log.level` | `uperf-config/src/switcher.rs`、`uperf-core/src/{inotify,watch_task}.rs`、`docs/m6b-evidence.md` | **已达成**（真机）：`Preset inode -> 'x'` → `Preset 'a' -> 'b'` → `preset applied writes=16`（与 M4 同 16 条）；`auto` 走 perapp；未定义值报上游原串；hint 字节 `transitioned=true`；`log.level` info/debug 双验；SIGTERM 干净 |
| **M6c** ✅ | `modules.input.*` 接入（唯一一处 vendored 改动）、`killall` 停止路径的真 bug | `cpp/dfps/source/modules/input_listener.{h,cpp}`、`cpp/uperf/{app_main,bridge}.cpp`、`docs/m6b-evidence.md` §9-§11 | **已达成**：真机 `killall uperf` 后 governor 回 `powersave`、0 残留（修复前 2 残留 + 永久卡 `userspace`）；`swipeThd` 随配置 0.01/0.03 双验；DFPS_VENDOR.md 记录了唯一偏离并用上游 clone 实证 |
| **M6d** ✅ | `atrace`（复用 dfps 自己的 `utils/atrace.c`）、`pinned`+top-app 真机验证 | `cpp/uperf/bridge.cpp`、`rust/uperf-core/src/{ffi,lib}.rs`、`docs/m6d-evidence.md` | **已达成**：marker 机制由 vendored `atrace.c` 定死（格式 `B\|<pid>\|<tag>`），设备上取到 **34 条**来自本进程的真实 marker；`pinned` 语义真机验证（Settings 线程 90×bg → 209×idle，pinned 进程落 bg 次数 **0**）；`top=` 实测 launcher3 → settings |
| **M7** | 整机替换 `magisk/bin/uperf`、真机装 `libsfanalysis.so` 观察 hint 生产端与 atrace marker | 可发布的 Magisk zip | §10.4 全部 7 张基线通过；§1 成功判据全绿 |

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
| `sfanalysis.hint` 的状态码语义 | UNKNOWN | 真机抓取（SfAnalysisListener 落地时同步做） |
| **悬空类别引用 / 逗号 cpumask** | **已解**：3 份配置有真实缺陷（见上），按"容忍 + 记录 Anomaly"处理，白名单钉死 | `docs/m6-sched-evidence.md` §3 |
| **worker 里 `std::process::Command` 报 `ECHILD`** | **已解**：vendored dfps 监督器装了 `SIGCHLD`→`wait()` 处理器，worker 继承后**偷走并回收** Rust 的子进程。worker 不监督任何东西，故在 `uperf_rs_start` 重置为 `SIG_DFL` | `docs/m6-sched-evidence.md` §4 |
| **空替换会让正则匹配一切** | **已解 + 加锁**：home 解析失败曾返回空串，`/HOME_PACKAGE/`→`""` 使 Launcher 规则匹配全系统（3400 条判定）。三重防护：空结果视为失败、失败时保留字面 token、`SchedPlanner` 直接拒绝空 process pattern | `docs/m6-sched-evidence.md` §5 |
| **Android 自己的 task-profile 控制器会改写亲和性/SCHED 类** | **已解（方法层面）**：新进程 1–2s 内被 Android 归类（实测落到 cpus 0-3 / SCHED_IDLE，恰好与配置同值→曾造成假阳性）。**真机调度实验必须使用与 Android 取值不相交的目标值**，并做对照实验 | `docs/m6-sched-evidence.md` §7 |
| **`comm` 被截断到 15 字符**（`com.android.launcher3` → `com.android.lau`） | **已知**：`/MAIN_THREAD/` 用观测到的 `comm` 替换，因此自洽（pattern 与线程名同源） | `docs/m6-sched-evidence.md` §7 |
| **无 CAP_SYS_NICE 时不能把调度类"升"回去**（NORMAL→IDLE 可以，IDLE→NORMAL EPERM） | **已解**（内核真实规则，非沙箱怪癖）；host 单测按此写成"接受两种结果" | `docs/m6-sched-evidence.md` §6 |
| `/proc/<tid>/stat` 字段 18 是 `priority`(=20+nice)，**nice 是字段 19** | **已解**：曾读 index 15 得到 25（nice=5 时） | `docs/m6-sched-evidence.md` §6 |
| **`atrace` 的 marker 载荷** | **已解**：payload 由 vendored `cpp/dfps/source/utils/atrace.c` 定义（`B\|<pid>\|<tag>`/`E\|<pid>`/`C\|<pid>\|<tag>\|<n>`），开关就是 `AtraceToggle()`；**marker 字面量本就不该出现在二进制里**（由埋点处的 `ATRACE_*` 宏构造，埋点在 `inotify.cpp:59`/`topapp_monitor.cpp:50`）。设备实测 34 条真实 marker | `docs/m6d-evidence.md` §1 |
| **`sfanalysis.hint` 的生产端路径** | **UNKNOWN**：`libsfanalysis.so` 里**没有任何路径串**（只有 `/proc/<pid>/comm|stat`、`/proc/self/maps`、`/system/bin/surfaceflinger`），生产端必然另经他途取得路径。消费端按 `<config 目录>/sfanalysis.hint` 实现（[I]） | `docs/m6b-evidence.md` §8 |
| **`modules.input.*` 未接入** | **已知 parity 缺口**：`swipeThd/gestureThdX/gestureThdY/gestureDelayTime/holdEnterTime` 仍是 vendored `input_listener.cpp` 的硬编码默认值（`0.01/0.03/0.03/2.0/1.0`），而二进制里这 5 个键**确实存在**（上游会读）。sdm888 的值恰好等于默认值，所以本机看不出来，其他配置会有差异 | 因阈值是 private 且无 setter，改它必须动 `cpp/dfps/**`（违反 M0 的"零改动"不变量）。M7 二选一：(a) 加 setter 并在 `DFPS_VENDOR.md` 记录该 diff；(b) 在 `cpp/uperf/` 侧重写 InputListener |
| **`auto` 的语义** | [I]：`cur_powermode.txt` 的合法值之一但非预设名；二进制有 `Internal perapp switcher {}`/`Internal perapp switcher cannot be enabled`，故读作"交给 perapp 规则" | `docs/m6b-evidence.md` §1 |
| **`UPERF_FAKE_ROOT` 只覆盖 sysfs 写入** | **已知**：`sched_setaffinity`/`sched_setscheduler` 是 syscall，无路径可重定向。真机调度测试必须同时设 `UPERF_SCHED_DRY_RUN=1` | `docs/m6b-evidence.md` §7 |
| **`killall uperf` 会把 governor 永久留在 `userspace`** | **已解**：`SIGTERM` 同时到达 daemon 与 worker，daemon 再转发 `SIGUSR1` ⇒ worker 的信号处理函数**在同一线程重入**，两次 `uperf_rs_stop()` 争同一个**不可重入**日志 Mutex → 死锁，进程不退出。三处修：handler 同时处理 `SIGTERM/SIGINT`；handler 入口用 `sigprocmask` 屏蔽这三个信号；所有日志辅助改用 `try_lock` 并丢弃（丢日志远好于挂死 daemon） | `docs/m6b-evidence.md` §9 |
| **`modules.input.*` 曾未接入** | **已解**：README 标 `gestureDelayTime`/`holdEnterTime` 为"暂不使用"，故只接 3 个；`swipeThd` 在 62/63 份配置里是 vendored 默认值的 **3 倍**（0.03 vs 0.01），只有开发用的 sdm888.json 恰好是 0.01 所以本机看不出来。这是 `cpp/dfps/**` **唯一**的改动（3 行 setter），已实证 `diff -rq` 只差那两个文件 | `docs/m6b-evidence.md` §11、`cpp/dfps/DFPS_VENDOR.md` |
| **`modules.input.enable` 未生效** | **UNKNOWN**：63 份配置全为 true，且 vendored 监听器在配置解析前就由平台层启动；配置若禁用只会打日志不会真的不启动 | `docs/m6b-evidence.md` §11 |
| **`/sdcard` 下 `unlink` 对 root 静默失效**（`rm` 返回 0 但文件仍在） | **已解**：走底层 `/data/media/0/...` 删除即可（FUSE 视图同步清除）。这也解释了为何 5 份配置用 `/data/media/0/Android/yc/uperf/...` 而非 `/sdcard/...` | `docs/m6b-evidence.md` §7 |
| **`offscreen.state` 在 alioth 上从未触发**：vendored 判据是 `/dev/cpuset/restricted` pid 数 > 10，而该集合在本机恒为 0 | **已解（M1 真验）**——电源键物理熄屏后 `restricted` 立即从 0 涨到 221，`Rust: offscreen.state = true` 出现。`input keyevent 26` 在某些场景下不会真正熄屏，必须真的按电源键。 |
| `/dev/cpuset` 在本机是 **cgroup v1 形态**（有 `tasks`/`notify_on_release`），`/sys/fs/cgroup` 是 v2（只有 `apps`/`system`） | **已解（静态+真机）**——v3 二进制只读 `tasks`，写 `cpus`（不是 `tasks`！），fd 15-19 = `cpuset/{background,foreground,restricted,system-background,top-app}/cpus`，所以 cgroup v1 是真视图。**M3 的 sysfs 写入器必须新增 cpuset cpus (cpu mask) 子类型。** |
| `topapp.pkgName` 的 `|Δpid| > 10` 门槛（`TOP_TASK_NR_DIFF_MIN`）是否与原版一致 | **已解**——dfps vendored `topapp_monitor.cpp` 与原版同源，原版**不复写**该模块，门槛=10 与 dfps 一致 |
| NDK r26（clang 21）下 vendored scnlib / spdlog 需要非侵入式 shim | 已解决 | `cpp/CMakeLists.txt` 三处注释 + `docs/m0-evidence.md` §1（未改动 `cpp/dfps/**` 任何字节） |
| **新增**：原版写 `/dev/cpuset/<g>/cpus`（cpu mask），不是 cgroup v2 cpu.max | 已发现 | M3 sysfs 写入器必须新增 cpu-mask 子类型（详见 `docs/m1-static-reverse.md` §3-§4） |
| **SfHint 枚举值 ↔ 字符串映射** | **已静态推导出（6 值：idle/switch/trigger/gesture/touch/junk，对应 0..5；≥6 = unknown）** | 见 `docs/m1-static-reverse.md` §1.3；M3 hint state machine 按此实现 |
| **`presets` 的真实结构** | **已解**：扁平 `presets[preset][scene]`，无 `base_hint` | 见 §8.2 与 `docs/m2-evidence.md` |
| **stale `libuperf_core.a` 导致 ABI 错位崩溃** | 已修（工程性坑） | 症状：进程 `SIGSEGV` @ `memcpy(src=0x79,len=120)`；根因：改了 Rust 侧 FFI 签名但 `.a` 未重编，C++ 把 `len` 当指针。修法：`build.sh make` 现在**先跑 cargo 再 cmake**（见 `build.sh::build_rust`） |
| v3 的 Hint 命名与日志文案（v2 文档不可信） | UNKNOWN | 以原版真机日志为准（M2 起逐条采集） |
| `.data` 4 条不透明记录 | UNKNOWN | 不影响本项目（不重写该库） |
| 原版 `CpufreqWriter` 各平台子类的确切分支条件 | **已解**（机制层面）：8 个候选子类全部要求一个 **min-freq 节点**（`epic minfreq`/`msm minfreq`/`scaling min`），在 alioth 上全被驱动锁死 → 原版直接 `No CpufreqWriter supported for this platform` 退出。本实现改走 `scaling_governor=userspace` + `scaling_setspeed`（实测可用），见 §8.4.2 | 已记录 `docs/m5-cpu-governor.md` §2 |
| **上游打印的 OPP 列表是设备表的子集**（cluster0 打了 9/17），规则拟合不出（非成本去重/非凸包/非功率阈值） | **UNKNOWN**（疑为当时抓取残缺） | 本实现用**全量**设备 OPP 表（上游可选集的超集），不影响可达频点范围 |
| 上游功耗受限时的**分配规则**（是否也做边际成本等值） | UNKNOWN | 本实现按"边际成本相等 = 限定功耗下总容量最大"实现，属读法 [I]，非字节级对齐 |
| `SIGKILL` 情况下 governor 不会被 disarm（`scaling_max_freq` 驱动级只读，无从外部救援） | **已知隐患** | 进程内不可解（SIGKILL 后无代码可执行）；须由 magisk 模块的 `uninstall.sh`/启动自检还原 governor，**未实现** |
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
