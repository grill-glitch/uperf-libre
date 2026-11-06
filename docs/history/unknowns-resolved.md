# 已解决的未知项与事故（从 AGENT.md §12 迁出）

每条都是"当时不知道 / 当时会坏"，现在已经有结论。留在这里是为了不再重新推导一遍；
其中与代码行为相关的教训已经收进 `AGENT.md` §6/§8/§13 与
`uperf-rewrite-parity` 技能（后者是给动手改的人看的操作手册）。

照抄自 `AGENT.md` §12 的原样内容（其中仍然开放的部分已抽到 AGENT.md §12）：

---


### 12.1 SfAnalysis 的 hook 点（**M8 起重写,0 闭源 blob**）

**目标（M8 重写计划）**：在 uperf 模块发布的二进制树中**移除所有闭源 `.so`**。
SfAnalysis 的 SF 注入库 `libsfanalysis.so`（26 KB / 47 函数、纯 C、静态链 xHook）
由本仓库**自研 Rust 重写**，新库名 `libsfanalysis_rs.so`。**注意**：vendor 件其实
**不写** `sfanalysis.hint`（见下），所以"接口契约"实际上是"在 SF 进程内装上 4 个 hook
并保持宿主存活"，重写件按同一语义实现。

**逆向现状（m1-static-reverse.md §1.5 的结论已被 `docs/records/m8-sfanalysis-reverse.md` 推翻）**：
- `libsfanalysis.so` 通过 `patchelf --add-needed` 注入 surfaceflinger；
  解析 `/proc/self/maps` 定位 `/system/lib64/libandroidfw.so`，再按名挂钩。
- **被 hook 的是 4 个 libc 函数**：`ioctl` / `epoll_wait` / `pthread_cond_wait` /
  `pthread_cond_timedwait`（函数名字符串 TEA 加密在 `.data` 里，解密器见
  `scripts/sfanalysis-deobf.py`）。`xh_refresh_loop` 是 **xHook 自己的线程名**，
  不是 hook 目标——m1 §1.5 把它当成目标函数是错的。
- 机制是 **xHook 的 GOT/PLT 改写**（不是 inline patch）：改写调用方 GOT 槽，
  只需对数据页 `mprotect(RW)`。
- **它不写 `sfanalysis.hint`**：43 个未定义符号里没有 `write`/`pwrite`/`syscall`/
  `mmap`，真机 strace 也未见文件写。也就是说 **hint 在生产端根本不存在**
  （详见 `docs/records/m8-sfanalysis-reverse.md` §6）。重写件同样不写——这是复刻，不是缺功能。
- 本机（crDroid A16）的 SF **不加载 libandroidfw.so**，所以 vendor 件在 enforcing 下
  既不崩也不 hook——这是它自己的目标选择问题，与权限无关。
- 静态产物（vendor 二进制里保留全部字符串）：
  `/system/lib64/libandroidfw.so`（目标 .so）、
  `/system/lib64/libandroid.so`（备用）、
  `/system/bin/surfaceflinger`（进程 anchor）、
  `/proc/%d/comm`、`/proc/self/maps`、`/proc/%d/stat`、
  `xh_refresh_loop`（被 hook 函数名）。
- v3 二进制里的 4 条 13/21/29/29 字节不透明 `.data` 记录
  （均以 `0x9e3772XX` 开头、`0xb5` 结尾）—— **这条 v3 说的是 uperf 主二进制**，
  不是 libsfanalysis；libsfanalysis 内已见到明文符号名。

**M8 重写交付物**：
1. `magisk/bin/libsfanalysis_rs.so`：Rust 实现（同 26 KB 量级），NDK r30 toolchain。
2. **不依赖 xHook**：自写 mprotect + 跳转（PLT/GOT 不适用——目标在 RWX 外的 .text，
   需要 inline patch），保留 vendor 件的"hook on first call"语义。
3. **SfHint 6 值 FSM 与 §1.3 已对齐**（idle/switch/trigger/gesture/touch/junk，
   ≥6 = unknown）；M8 真机抓 byte 序列，验证每条 hint 触发条件与 vendor 件一致。
4. **magisk 注入路径**：`customize.sh` 在 `setup.sh` 之前执行 `patchelf --add-needed
   libsfanalysis_rs.so $MODPATH/system/bin/surfaceflinger`；SF 启动时 dlopen 拉入。
   （surfaceflinger 是 magisk 注入体系的常规目标，与 vendor 件路径同。）

**约束（不可破）**：
- **不能改** §7.4 的 hint 文件协议（路径 / 单字节 / 0..5 枚举）—— 消费端已经按
  vendor 件字节协议实现，改了 SfAnalysisListener 必须同步动，违反 §7 不可变清单。
- **不能动** `cpp/dfps/**`—— libsfanalysis_rs 是新模块，不是 dfps 的复制品。
- **`magisk/bin/uperf` 仍是 Rust 重写产物**（M0 已完成），不含闭源 blob。

**进度追踪**：`docs/records/m8-sfanalysis-reverse.md`（r2 静态逆向后写入），
`docs/spec/sfanalysis.md`（边界 spec），AGENT.md §11 milestone 表 M8 行。
**不做 Frida**：本仓库 §7.1 / §10.5 不要求动态验证，r2 静态 + 真机 byte 抓取已够。
**状态码→语义映射**：M2 起逐条采集（与现有 §12.2 第 1 行同项，M8 完成后转为已解）。

**M8 真机测试发现（2026-10-09，alioth/A16，见 `docs/records/m8-sfanalysis-reverse.md` §0/§1.4/§2）**：
r2 深挖推翻了 M1 的 hook 目标结论——`xh_refresh_loop` 是 **xHook 自己的后台线程名**
（`pthread_setname_np`），不是被 hook 的函数。真正的 4 个 hook 目标以
TEA 变体加密在 `.data`，已解密：**`ioctl` / `epoll_wait` /
`pthread_cond_timedwait` / `pthread_cond_wait`**（libc 函数，解密器见
`scripts/sfanalysis-deobf.py`）。当前 `rust/uperf-sfanalysis` 的
`HOOK_TARGET_SYM = "xh_refresh_loop"` **是错的**，且 `patch_target` 会写到
libandroidfw 的 **ELF 头段**（maps 首条 r--p）而非 `.text`——真机 harness 已实证
该段被 trampoline 覆写、perms 变 `rwxp`。**M8 未完成，需按新目标重写。**

**另一条真机事实**：A16 的 `surfaceflinger` 根本不加载 `libandroidfw.so`
（431 个 .so + DT_NEEDED 双证），故 vendor 件在这台设备上同样静默失效。

**M8 前提被推翻（2026-10-09，三证见 `docs/records/m8-sfanalysis-reverse.md` §6）**：
`sfanalysis.hint` **没有生产者**。两份 vendor 库（`libsfanalysis.so` /
`libssanalysis.so`）的未定义符号里**没有 `write` / `syscall` / `mmap` / `pipe`**，
即没有写文件或建 IPC 的能力；`strace -f` 全量追踪（harness 1733 行 + 运行中 daemon
29129 行）确认零文件写入，daemon 只是在轮询一个永不存在的文件
（`openat(hint, O_RDONLY) = ENOENT`）。所以"重写 hint 生产者"**没有可对齐的对象**。
处置路线（删 blob / 移植进程内行为 / 新增 opt-in 生产者）见
`docs/spec/sfanalysis.md §8`，**待定**。当前 `rust/uperf-sfanalysis` 在选定方向前
不接入发布流程。

### 12.2 其它未知
| 项 | 状态 | 处理 |
|---|---|---|
| `sfanalysis.hint` 的状态码语义 | UNKNOWN | M8 完成后转已解（见 `docs/records/m8-sfanalysis-reverse.md` §3） |
| **悬空类别引用 / 逗号 cpumask** | **已解**：3 份配置有真实缺陷（见上），按"容忍 + 记录 Anomaly"处理，白名单钉死 | `docs/records/m6-sched-evidence.md` §3 |
| **worker 里 `std::process::Command` 报 `ECHILD`** | **已解**：vendored dfps 监督器装了 `SIGCHLD`→`wait()` 处理器，worker 继承后**偷走并回收** Rust 的子进程。worker 不监督任何东西，故在 `uperf_rs_start` 重置为 `SIG_DFL` | `docs/records/m6-sched-evidence.md` §4 |
| **空替换会让正则匹配一切** | **已解 + 加锁**：home 解析失败曾返回空串，`/HOME_PACKAGE/`→`""` 使 Launcher 规则匹配全系统（3400 条判定）。三重防护：空结果视为失败、失败时保留字面 token、`SchedPlanner` 直接拒绝空 process pattern | `docs/records/m6-sched-evidence.md` §5 |
| **Android 自己的 task-profile 控制器会改写亲和性/SCHED 类** | **已解（方法层面）**：新进程 1–2s 内被 Android 归类（实测落到 cpus 0-3 / SCHED_IDLE，恰好与配置同值→曾造成假阳性）。**真机调度实验必须使用与 Android 取值不相交的目标值**，并做对照实验 | `docs/records/m6-sched-evidence.md` §7 |
| **`comm` 被截断到 15 字符**（`com.android.launcher3` → `com.android.lau`） | **已知**：`/MAIN_THREAD/` 用观测到的 `comm` 替换，因此自洽（pattern 与线程名同源） | `docs/records/m6-sched-evidence.md` §7 |
| **无 CAP_SYS_NICE 时不能把调度类"升"回去**（NORMAL→IDLE 可以，IDLE→NORMAL EPERM） | **已解**（内核真实规则，非沙箱怪癖）；host 单测按此写成"接受两种结果" | `docs/records/m6-sched-evidence.md` §6 |
| `/proc/<tid>/stat` 字段 18 是 `priority`(=20+nice)，**nice 是字段 19** | **已解**：曾读 index 15 得到 25（nice=5 时） | `docs/records/m6-sched-evidence.md` §6 |
| **`atrace` 的 marker 载荷** | **已解**：payload 由 vendored `cpp/dfps/source/utils/atrace.c` 定义（`B\|<pid>\|<tag>`/`E\|<pid>`/`C\|<pid>\|<tag>\|<n>`），开关就是 `AtraceToggle()`；**marker 字面量本就不该出现在二进制里**（由埋点处的 `ATRACE_*` 宏构造，埋点在 `inotify.cpp:59`/`topapp_monitor.cpp:50`）。设备实测 34 条真实 marker | `docs/records/m6d-evidence.md` §1 |
| **`sfanalysis.hint` 的生产端** | **已解（真机证伪"有生产者"）**：vendor `libsfanalysis.so` / `libssanalysis.so` 的未定义符号里**无 `write`/`syscall`/`mmap`/`pipe`**（不能写文件/建通道）；harness `strace -f`（1733 行）写标志 open = 0；运行中 daemon 的 `strace`（29129 行）只见到 `openat(hint, O_RDONLY) = ENOENT` 轮询。**该文件没有生产者**——上游此特性未完成/已废弃 | `docs/records/m8-sfanalysis-reverse.md` §6 |
| **`modules.input.*` 未接入** | **已知 parity 缺口**：`swipeThd/gestureThdX/gestureThdY/gestureDelayTime/holdEnterTime` 仍是 vendored `input_listener.cpp` 的硬编码默认值（`0.01/0.03/0.03/2.0/1.0`），而二进制里这 5 个键**确实存在**（上游会读）。sdm888 的值恰好等于默认值，所以本机看不出来，其他配置会有差异 | 因阈值是 private 且无 setter，改它必须动 `cpp/dfps/**`（违反 M0 的"零改动"不变量）。M7 二选一：(a) 加 setter 并在 `DFPS_VENDOR.md` 记录该 diff；(b) 在 `cpp/uperf/` 侧重写 InputListener |
| **`auto` 的语义** | [I]：`cur_powermode.txt` 的合法值之一但非预设名；二进制有 `Internal perapp switcher {}`/`Internal perapp switcher cannot be enabled`，故读作"交给 perapp 规则" | `docs/records/m6b-evidence.md` §1 |
| **`UPERF_FAKE_ROOT` 只覆盖 sysfs 写入** | **已知**：`sched_setaffinity`/`sched_setscheduler` 是 syscall，无路径可重定向。真机调度测试必须同时设 `UPERF_SCHED_DRY_RUN=1` | `docs/records/m6b-evidence.md` §7 |
| **`killall uperf` 会把 governor 永久留在 `userspace`** | **已解**：`SIGTERM` 同时到达 daemon 与 worker，daemon 再转发 `SIGUSR1` ⇒ worker 的信号处理函数**在同一线程重入**，两次 `uperf_rs_stop()` 争同一个**不可重入**日志 Mutex → 死锁，进程不退出。三处修：handler 同时处理 `SIGTERM/SIGINT`；handler 入口用 `sigprocmask` 屏蔽这三个信号；所有日志辅助改用 `try_lock` 并丢弃（丢日志远好于挂死 daemon） | `docs/records/m6b-evidence.md` §9 |
| **`modules.input.*` 曾未接入** | **已解**：README 标 `gestureDelayTime`/`holdEnterTime` 为"暂不使用"，故只接 3 个；`swipeThd` 在 62/63 份配置里是 vendored 默认值的 **3 倍**（0.03 vs 0.01），只有开发用的 sdm888.json 恰好是 0.01 所以本机看不出来。这是 `cpp/dfps/**` **唯一**的改动（3 行 setter），已实证 `diff -rq` 只差那两个文件 | `docs/records/m6b-evidence.md` §11、`cpp/dfps/DFPS_VENDOR.md` |
| **`modules.input.enable` 未生效** | **UNKNOWN**：63 份配置全为 true，且 vendored 监听器在配置解析前就由平台层启动；配置若禁用只会打日志不会真的不启动 | `docs/records/m6b-evidence.md` §11 |
| **watcher 目录 watch 与自身日志形成反馈闭环**（跑满一个核） | **已解**：目录 watch 的 mask 含 `IN_CLOSE_WRITE\|IN_MODIFY`，而日志就在该目录 ⇒ 写日志触发监听→重读→再写日志。改为只报结构性事件（`IN_CREATE\|IN_MOVED_TO\|IN_DELETE`），内容写入交给文件自身 watch。**1065→68 ticks/10s** | `docs/records/m7-evidence.md` §1.1 |
| **userspace 接管会让小核掉到最低档**（实测 691200 vs 系统 1804800） | **已解**：接管改**默认关闭**（`UPERF_CPU_GOVERNOR=1` 才启用），因为上游 governor 在本内核上本就起不来、无 parity 可保；且不再臆造 `powersave`（alioth 真实默认是 `schedutil`，臆造等于把 CPU 永久钉在最低频） | `docs/records/m7-evidence.md` §1.2 |
| **`/sdcard` 下 `unlink` 对 root 静默失效**（`rm` 返回 0 但文件仍在） | **已解**：走底层 `/data/media/0/...` 删除即可（FUSE 视图同步清除）。这也解释了为何 5 份配置用 `/data/media/0/Android/yc/uperf/...` 而非 `/sdcard/...` | `docs/records/m6b-evidence.md` §7 |
| **`offscreen.state` 在 alioth 上从未触发**：vendored 判据是 `/dev/cpuset/restricted` pid 数 > 10，而该集合在本机恒为 0 | **已解（M1 真验）**——电源键物理熄屏后 `restricted` 立即从 0 涨到 221，`Rust: offscreen.state = true` 出现。`input keyevent 26` 在某些场景下不会真正熄屏，必须真的按电源键。 |
| `/dev/cpuset` 在本机是 **cgroup v1 形态**（有 `tasks`/`notify_on_release`），`/sys/fs/cgroup` 是 v2（只有 `apps`/`system`） | **已解（静态+真机）**——v3 二进制只读 `tasks`，写 `cpus`（不是 `tasks`！），fd 15-19 = `cpuset/{background,foreground,restricted,system-background,top-app}/cpus`，所以 cgroup v1 是真视图。**M3 的 sysfs 写入器必须新增 cpuset cpus (cpu mask) 子类型。** |
| `topapp.pkgName` 的 `|Δpid| > 10` 门槛（`TOP_TASK_NR_DIFF_MIN`）是否与原版一致 | **已解**——dfps vendored `topapp_monitor.cpp` 与原版同源，原版**不复写**该模块，门槛=10 与 dfps 一致 |
| NDK r26（clang 21）下 vendored scnlib / spdlog 需要非侵入式 shim | 已解决 | `cpp/CMakeLists.txt` 三处注释 + `docs/records/m0-evidence.md` §1（未改动 `cpp/dfps/**` 任何字节） |
| **新增**：原版写 `/dev/cpuset/<g>/cpus`（cpu mask），不是 cgroup v2 cpu.max | 已发现 | M3 sysfs 写入器必须新增 cpu-mask 子类型（详见 `docs/records/m1-static-reverse.md` §3-§4） |
| **SfHint 枚举值 ↔ 字符串映射** | **已静态推导出（6 值：idle/switch/trigger/gesture/touch/junk，对应 0..5；≥6 = unknown）** | 见 `docs/records/m1-static-reverse.md` §1.3；M3 hint state machine 按此实现 |
| **`presets` 的真实结构** | **已解**：扁平 `presets[preset][scene]`，无 `base_hint` | 见 §8.2 与 `docs/records/m2-evidence.md` |
| **stale `libuperf_core.a` 导致 ABI 错位崩溃** | 已修（工程性坑） | 症状：进程 `SIGSEGV` @ `memcpy(src=0x79,len=120)`；根因：改了 Rust 侧 FFI 签名但 `.a` 未重编，C++ 把 `len` 当指针。修法：`build.sh make` 现在**先跑 cargo 再 cmake**（见 `build.sh::build_rust`） |
| v3 的 Hint 命名与日志文案（v2 文档不可信） | UNKNOWN | 以原版真机日志为准（M2 起逐条采集） |
| `.data` 4 条不透明记录 | **已分类为 uperf 主二进制特性**：与 libsfanalysis 无关；M8 不再 carry vendor libsfanalysis，故不影响 | `docs/records/m1-static-reverse.md` §1.5 |
| 原版 `CpufreqWriter` 各平台子类的确切分支条件 | **已解**（机制层面）：8 个候选子类全部要求一个 **min-freq 节点**（`epic minfreq`/`msm minfreq`/`scaling min`），在 alioth 上全被驱动锁死 → 原版直接 `No CpufreqWriter supported for this platform` 退出。本实现改走 `scaling_governor=userspace` + `scaling_setspeed`（实测可用），见 §8.4.2 | 已记录 `docs/records/m5-cpu-governor.md` §2 |
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

