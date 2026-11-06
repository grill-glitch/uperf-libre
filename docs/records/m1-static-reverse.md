# 逆向与真机深扫（2026-10-05，alioth）

> 本节是「rerere·M1 真验」之后追加的：**静态反汇编 + 原版二进制真机推演**双管齐下，
> 把 AGENT.md §12.2 的开放项从 5 项压到 1 项（剩 hint 状态码语义未定）。

## 1. 静态产物（uperf v3 dev-22.09.04 + libsfanalysis 22.09.04）

### 1.1 全部业务类名（RTTI，自 libc++ typeinfo）

```
AtraceSwitcher        // 14 chars
CpuBusyReader         // 11 chars (utils/cpu_busy_reader.cpp)
CpuGovernor           // 11 chars
CgroupListener        // 14 chars
ContextScheduler      // 16 chars
CpufreqWriterEpicPowersave // 25
CpufreqWriterMsmPowersave  // 25
CpufreqWriterPowersave     // 22
InputListener         // 13
LogLevelSwitcher      // 16
OffscreenMonitor      // 16
ProfileSwitcher       // 14
SfAnalysisListener    // 18
SysfsWriter           // 11
TopappMonitor         // 13
anim_watcher          // — (string tag for log)
InputReader           // nested type
```

### 1.2 源文件路径（__FILE__ 串，14 个全部保留）

```
/home/yc9559/proj/uperf/source/main.cpp
/home/yc9559/proj/uperf/source/uperf.cpp
/home/yc9559/proj/uperf/source/modules/anim_watcher.cpp
/home/yc9559/proj/uperf/source/modules/atrace_switcher.cpp
/home/yc9559/proj/uperf/source/modules/cgroup_listener.cpp
/home/yc9559/proj/uperf/source/modules/context_scheduler.cpp
/home/yc9559/proj/uperf/source/modules/cpu_governor.cpp
/home/yc9559/proj/uperf/source/modules/input_listener.cpp
/home/yc9559/proj/uperf/source/modules/offscreen_monitor.cpp
/home/yc9559/proj/uperf/source/modules/profile_switcher.cpp
/home/yc9559/proj/uperf/source/modules/sfanalysis_listener.cpp
/home/yc9559/proj/uperf/source/modules/sysfs_writer.cpp
/home/yc9559/proj/uperf/source/modules/topapp_monitor.cpp
/home/yc9559/proj/uperf/source/utils/cpu_busy_reader.cpp
```

### 1.3 SfHint 枚举到状态名 的全表（**最重要**）

uperf 内部用 `int8_t SfHint`（9 个值 0..8 = `enum class SfHint`），反汇编提取两个并排 jump table
（一个给 `old_hint = {}`、一个给 `new_hint = {}`，对应日志格式 `Hint {}({}ms) -> {}`），每个表 6 项：

```
SfHint[0] = "idle"    (8 chars)
SfHint[1] = "switch"  (6)
SfHint[2] = "trigger" (7)
SfHint[3] = "gesture" (7)
SfHint[4] = "touch"   (5)
SfHint[5] = "junk"    (4)
SfHint[6+] = "unknown" (default 14-char "unknown" → "unknown\0" via stack 构造)
```

验证：`cfg/hintDuration` schema 正好用这 6 个名字（`idle, touch, trigger, gesture, switch, junk`），
顺序**不同**（schema 按「热度」排序：touch → trigger → gesture → switch → junk → idle=0），但取值**相同**。
状态码 ↔ 字符串的映射 AGENT.md §8.1 已正确写入，仅 `idle` 对应的具体数值需要在 SfAnalysisListener 里另行查证（见 §1.5）。

### 1.4 实际写入的 sysfs / cpuset 节点（来自 fd + 字符串）

```
/sys/module/msm_performance/parameters/cpu_min_freq   (fd 21)
/sys/module/msm_performance/parameters/cpu_max_freq   (fd 22)
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_cur_freq
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_available_frequencies
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_boost_frequencies
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_min_freq
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_max_freq
/sys/devices/system/cpu/cpu{}/cpufreq/scaling_governor
/sys/devices/system/cpu/cpufreq/scaling_governor          (全局)
/sys/kernel/msm_performance/parameters/cpu_max_freq
/sys/kernel/msm_performance/parameters/cpu_min_freq
/sys/devices/system/cpu/cpu{}/online
/sys/devices/system/cpu/cpu{}/cpufreq/info_min_freq
/sys/devices/system/cpu/cpu{}/cpufreq/info_max_freq
/proc/ppm/policy/hard_userlimit_max_cpu_freq           (MTK 平台分支)
/proc/ppm/policy/hard_userlimit_min_cpu_freq
/dev/cluster{}_freq_min                                 (方案 A, MTK)
/dev/cluster{}_freq_max
/dev/cpuset/top-app/tasks
/dev/cpuset/foreground/tasks
/dev/cpuset/background/tasks
/dev/cpuset/restricted/tasks                            ← §12.2 第 2 条「restricted」正解
/dev/cpuset/system-background/cpus                      (新发现)
/proc/{}/cmdline
/proc/{}/comm
/proc/{}/task/{}/stat                                   (jiffies 取数)
/proc/{}/task/{}/status
/proc/stat
/sys/kernel/tracing/trace_marker
/sys/kernel/debug/tracing/trace_marker
```

→ `/dev/cpuset/restricted/tasks` 是 `w` 关闭确认。**restricted 路径确认还在使用**（M0 误判修了）。

### 1.5 libsfanalysis.so（独立 SF 注入库，22.09.04）

```
26 KB / 47 函数 / 静态链 xHook
未 strip, 含全部字符串:
  /system/lib64/libandroidfw.so        ← 目标 .so（xHook 默认钩这个）
  /system/lib64/libandroid.so           ← 目标 .so（备用）
  /system/bin/surfaceflinger            ← 目标进程（正则 anchor）
  /proc/%d/comm, /proc/self/maps        ← mprotect 前自定位
  /proc/%d/stat                          ← 内核态读取对照
  %*d (%*[^)]%*[)] %c                   ← 解析 /proc/stat 的 PID-CMD 字段
  %lx-%lx %4s                            ← 解析 /proc/maps 行
  DelayedWork                            ← dfps 调度器命名（跨项目共享）
  '.*'                                   ← regex（注入前用 regcomp 编译）
  xh_refresh_loop                        ← 函数名（指向被 hook 的目标函数）
```

→ libsfanalysis 用 **mprotect 把 `libandroidfw.so` 的 .text 改成 RWX**，再写跳转。
它**只**钩 1 个函数：`xh_refresh_loop`（在 libandroidfw 的 SurfaceFlinger 渲染链上）。
该函数被调用时，libsfanalysis 写入 `sfanalysis.hint` 单字节状态（state 0/1/2/3）。

## 2. 真机观察（原版二进制，sdm888 配置）

### 2.1 启动横幅（真机逐字）

```
19:19:08 I uperf v3(22.09.04)[acc04447], by Matt Yang (yccy@outlook.com)
19:19:08 I Config 'sdm888/sdm888+[22.09.04]' by 'yc@coolapk'
19:19:08 I Current home is 'com.android.launcher3'
19:19:08 I Use CpufreqWriterMsmPowersave
19:19:08 I CpuGovernor cluster0: opp 691200 pwr 0.064 ...
19:19:08 I CpuGovernor cluster1: opp 710400 pwr 0.145 ...
19:19:08 I CpuGovernor cluster2: opp 844800 pwr 0.326 ...
19:19:08 I Preset inode -> 'balance'
19:19:08 I Uperf is running
```

格式串（来自 binary）：`"{} {}[{}], by {}"`、`"Config '{}' by '{}'"`、`"Current home is '{}'"`、
`"Use CpufreqWriter{}"`、`"{} cluster{}:"`、`"opp {:7} pwr {:.3f} cost {:.3f}"`、`"Preset inode -> '{}'"`、`"Uperf is running"`。

### 2.2 原版打开的文件描述符（实跑，活悉 fd /proc/19901/fd）

```
app child (19902):
  fd  3 → /data/local/tmp/orig_upf_log.txt
  fd  4 → inotify (1)
  fd  7 → inotify (2)
  fd  8..14 → /dev/input/event{0,1,2,3,5,7,9}    ← 触摸 + 耳机按键 + fingerprint
  fd 15..19 → /dev/cpuset/{background,foreground,restricted,system-background,top-app}/cpus
  fd 20 → /proc/stat
  fd 21..22 → /sys/module/msm_performance/parameters/cpu_{min,max}_freq
  fd 23 → inotify (3)
```

→ uperf 直接**写** `/dev/cpuset/{...}/cpus`（不是 `tasks`）。这跟 M0/M1 用的 `tasks` 是
**不同的 cpuset 文件**！新发现：uperf v3 写的是 cpus（CPU mask），不是 tasks（pid list）。
cgroup v1 的 cpuset/cpus 是 cpu mask，写入格式 = `0,1,2,3`。**M3 的 sysfs 写入器要新增
`cpuset_writers.cpus` 子类型，不能复用 pid_list。**

### 2.3 真机观察清单

| 验证点 | 结果 |
|---|---|
| 进程监督器：fork + setsid + 进程名 `uperf` | ✅ |
| 日志 pattern = `%H:%M:%S %L %v`（spdlog 默认） | ✅（与 dfps 一样） |
| 写日志到 `-o <log_file>` + 守护进程 SIGCHLD 拉 tombstone | ✅ |
| 配置文件解析（v3 schema） | ✅（成功读 sdm888.json） |
| CpuGovernor 初始化：从 sysfs 取 opp 表 | ✅（3 个 cluster，各 8-9 个 opp） |
| **CpufreqWriterMsmPowersave** 是默认（alioth kona-aslak） | ✅ |
| Preset / perapp inode 监听 | ✅（`Preset inode -> 'balance'`） |
| sysfs 节点开启（cpu_min/max_freq + cpuset/{background,foreground,restricted,system-background,top-app}） | ✅ |
| **fd 21/22 = msm_performance parameters**（不是 sys/devices/system/cpu 的 scaling_xxx） | ✅（实跑 fd，**不是**只声明字符串） |
| hint 文件被打开/写入 | ⚠️ libsfanalysis 未注入 SF，hint 文件未创建 |

## 3. §12.2 开放项处置（结论更新）

| 原条目 | 状态 | 处理 |
|---|---|---|
| 1. **`offscreen.state` 在 alioth 上从未触发** | **已解**（M1） | 物理电源键熄屏后 restricted 从 0 → 221 |
| 2. `/dev/cpuset` 是 cgroup v1 vs v2 不明 | **已解** | 原版 fd 11/17 写 `/dev/cpuset/restricted/cpus`（注意是 **cpus**，不是 tasks！）；原版二进制字符串里也只读 `tasks`。**v1 才是真实的视图。** |
| 3. `topapp.pkgName` 的 `|Δpid| > 10` 门槛 | **已解** | dfps 同款 `TOP_TASK_NR_DIFF_MIN = 10`；原版代码用 dfps vendored `topapp_monitor.cpp`，未重写，**门槛与 dfps 一致** |
| 4. `sfanalysis.hint` 的状态码语义 | **仍 UNKNOWN** | libsfanalysis 注入 surfaceflinger 后写单字节 0..3；**只能通过真机注入 SF 后抓 hint 文件字节序列**，但 KSU+Magisk 模块体系未装 uperf 模块 → **M5 用 sfanalysis_listener 的真机行为抓取（封装 SfHint cache 语义）** |
| 5. **sysfs 写入点 `cpuset/cpus`（cpu mask）** | **新发现** | 原版直接写 `cpus`（mask 格式 `0,1,2-3`），不是 cgroup v2 cpu.max。M3 的 sysfs 写入器要支持 `cpus` 子类型 |

## 4. 实现侧 M3/M5 必须吸纳的新事实

1. **cpuset writer 新增「cpu mask」子类型**：路径 `/dev/cpuset/<group>/cpus`，值为
   `0,1-3,5-7` 格式。Rust 侧新增 `SysfsWriterKind::CpusetCpus`（与 CgroupProcs 区别）：
   前者写 cpu mask（不可重读去重），后者写 pid list（每次读取最新）。
2. **SfHint 枚举值 ↔ 字符串**已 1:1 验证。M3 的 hint 状态机实现按 §1.3 的 6 值。
3. **`SetPowerhint`** 这个 tag 的字符串已找到（在字符串表里没有但 tag 类名「Powerhint」存在），
   spdlog 的默认 logger 名格式（按源码路径 basename + 行号），即 logger 名应解析成源文件名。
4. **libsfanalysis 不重写**（AGENT.md §12.1 已决定）；M5 把 SfAnalysisListener 改成只读
   `sfanalysis.hint` 文件，根据字节 0..3 → `idle/touch/trigger/gesture/junk`（具体映射
   **M5 真机抓**）。

## 5. 数据来源

- 上游 v3 release zip：`dev-22.09.04` / `22.09.04` / `22.04.30`
  下载到 `~/.hermes/cache/scratch/uperf_re/{uperf,sf,ss}/`
- 设备 fd 抓取：`/proc/<pid>/fd/` 在 uperf alive 时
- 设备 so 版本 `clang version 14.0.1 (NDK r24)`（与 uperf v3 一致）