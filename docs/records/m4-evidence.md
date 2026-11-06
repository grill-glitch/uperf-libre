# M4 实施记录（hint FSM → config → sysfs 写入，真机端到端）

日期：2026-10-05 · 设备：alioth `f748d277`（crDroid A16 / KSU Next）

## 1. **重大发现：sysfs 路径来自配置，不在二进制里**

`strings` 扫描上游 `uperf` v3 二进制：**`devfreq` / `llcc` / `ufshc` / `qcom,` / `ddr` 全部 0 命中**。
说明路径不是硬编码的。翻配置后找到：

```jsonc
"modules": {
  "sysfs": {
    "enable": true,
    "knob": {                                   // ← 唯一的路径来源
      "cpusetTa":   "/dev/cpuset/top-app/cpus",
      "CPU4ddrmax": "/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq",
      "CPU4max":    "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq",
      "UFSmax":     "/sys/class/devfreq/1d84000.ufshc/max_freq"
    }
  }
}
```

**推论被纠正的地方**：
* M3 记录里那些「SoC 专属 devfreq 路径模板」是**我推的**，其实全部来自配置；
* `/proc/<pid>/fd` 看到的 `/sys/devices/platform/soc/soc:qcom,cpu4-llcc-ddr-lat/devfreq/...`
  是 `/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat` 这个**符号链接解析后**的目标；
* `CPU4ddrmax` 不是「cpu-cpu-llcc-bw」（我 M3 猜错了），配置里写的是
  `cpu4-llcc-ddr-lat`；`CPU7ddr*` 才是 `cpu-llcc-ddr-bw`。

写入器类型也不再需要猜（AGENT.md §8.3 那张表作废）：**从路径形状推断**：

| 路径形状 | WriterKind |
|---|---|
| `/dev/cpuset/*/cpus` | `CpusetCpus`（cpu mask） |
| `*//cgroup.procs`, `*/tasks` | `CgroupProcs`（pid list） |
| 含 `/cpufreq/` 且以 `_freq` 结尾 | `Cpufreq`（kHz，原样写） |
| `*/online` | `PerCpu` |
| 其余（devfreq `*_freq`、`msm_performance/*`、`/proc/ppm/*`） | `String` |

## 2. 架构收敛：共享 crate

M4 之前 schema/dispatch 逻辑有 **3 份副本**（`uperf-cli/config.rs`、`uperf-cli/plan.rs`、
`uperf-core/sysfs.rs`），已经漂移过一次。现在收成一个：

```
rust/uperf-config/           ← 新增，唯一真源
  src/config.rs              解析 + 层叠（resolve / all_sysfs_keys / sysfs_knob_table）
  src/sysfs.rs               dispatch / plan_scene / plan_for_config / WriterKind
rust/uperf-cli/              依赖 uperf-config（warn + plan 输出）
rust/uperf-core/             依赖 uperf-config（orchestrator 用它算真实写入）
```

## 3. 端到端链路（真机验证）

```
input.touch / offscreen.state / topapp.pkgName   (C++ 平台，复用 dfps)
   │  uperf_rs_on_event   (C ABI)
   ↓
Rust dispatcher (topic_dispatch)
   │  Event::parse
   ↓
Orchestrator
   │  HintState::process  → HintTransition{from,to}
   │  scene_for_hint      → "idle"/"touch"/"trigger"/"gesture"/"switch"/"junk"
   │  Config::resolve     → presets[mode][scene] > presets[mode]["*"] > initials
   │  plan_for_config     → modules.sysfs.knob 查路径 + WriterKind::from_path
   ↓
Sink
   ├ CollectingSink  (默认：只打日志)
   └ UnderRootSink   (UPERF_FAKE_ROOT 设置时：写 <root>/<path>，不碰真 sysfs)
```

真机日志（`UPERF_FAKE_ROOT=/data/local/tmp/uperf_fake`）：

```
20:31:23 I Rust: SysfsWrite path=/dev/cpuset/top-app/cpus value=0-7
20:31:23 I Rust: SysfsWrite path=/dev/cpuset/foreground/cpus value=0-2,4-7
20:31:23 I Rust: SysfsWrite path=/sys/class/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq value=5931
20:31:23 I Rust: SysfsWrite path=/sys/class/devfreq/soc:qcom,cpu-llcc-ddr-bw/max_freq value=5931
20:31:23 I Rust: SysfsWrite path=/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq value=2227200
20:31:23 I Rust: SysfsWrite path=/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq value=2496000
20:31:26 I Rust: fake-write root=/data/local/tmp/uperf_fake ok=16 failed=0
```

**fake 树 16 个文件全部落地**（`find /data/local/tmp/uperf_fake -type f | wc -l` → 16）。

## 4. 与上游 fd 轨迹的对齐

| 上游 v3 fd（`/proc/<pid>/fd`，15 个） | 我们的 plan（16 个） |
|---|---|
| cpu4-llcc-ddr-lat/{max,min}_freq | ✅ 同（配置里 `CPU4ddrmax/min`） |
| cpu-llcc-ddr-bw/{max,min}_freq | ✅ 同（`CPU7ddrmax/min`） |
| cpu-cpu-llcc-bw/{max,min}_freq | ✓ 我们的 `CPUllccmax/min` 指向它（配置如此） |
| cpufreq/policy4,7/scaling_max_freq | ✅ 同 |
| cpuset × 5 `/cpus` | ✅ 同 |
| msm_performance cpu_{min,max}_freq | ✓（上游 `initials` 里没有，`balance` 场景也未写；我们的 config 里 `cpuMax/cpuMin` 未在 sysfs.knob 声明 → 不产生写入） |
| — | ↔ 我们额外有 L3 max/min + UFSmax（上游也尝试了这两类，但因设备上节点名不匹配而 `W Knob ... not writeable`） |

写入去重：同一 node 被两个 knob 命中且值相同时只写一次（对应上游「与上一动作 diff，跳过相同值」）。

## 5. 上游在 alioth 上失败的两个 knob（我们也一样）

```
上游 20:12:27 W Knob '/sys/class/devfreq/18590100.qcom,cpu4-cpu-l3-lat/max_freq' not writeable
上游 20:12:27 W Knob '/sys/class/devfreq/18590100.qcom,cpu4-cpu-l3-lat/min_freq' not writeable
上游 20:12:27 W Knob '/sys/class/devfreq/1d84000.ufshc/max_freq' not writeable
```

设备实际暴露的节点：
```
/sys/class/devfreq/18590000.qcom,devfreq-l3:qcom,cpu4-cpu-l3-lat   ← 地址/命名都不同
/sys/devices/platform/soc/1d84000.ufshc                            ← 有，但没有 devfreq shim
```

→ **原版配置在这台机器上 L3/UFS 写不进去**。我们保持同样行为（不去「修复」路径），
因为 parity 的目标是复现上游行为，不是超越它。

## 6. 测试

| 命令 | 结果 |
|---|---|
| `cargo test -p uperf-config --release` | 6 passed（knob 表驱动、kind 推断、去重、fake-root、fd 轨迹覆盖） |
| `cargo test -p uperf-core --release` | 17 + 2 + 10 passed（含 6 个 orchestrator 端到端单测） |
| `cargo test -p uperf-cli --release` | 2 passed（38 + 63 配置） |
| `sh build.sh Release make check` | 全绿（974 KB、NEEDED `libc/libdl/libm`、stripped、BIND_NOW） |
| 真机 | 16 条 SysfsWrite 计划 + 16 个 fake 文件落地 |

## 7. M4 仍未做

| 项 | 说明 |
|---|---|
| 真 sysfs 写入 | 现在只写 fake root。真写需要：fd 缓存、`EACCES`/`ENODEV` 处理、cpufreq 的「new min > old max」重试（AGENT.md §8.3） |
| CPU governor 数值 | `demand = load + (1-load)*(margin+burst)`、PL1/PL2 池、sweetFreq 等尚未实现（M4 剩余部分） |
| context scheduler | PCRE2 规则引擎未接 |
| `cpufreq` 写入的 read-before-write | 上游会先读 `scaling_max_freq` 再决定是否写（去重 + 顺序约束） |
| `perapp` 切换 | `topapp.pkgName` 已能收，但还没接到 preset 切换 |
| `cur_powermode.txt` 热切换 | 现在只在启动时读一次；上游是 inotify 监听（C++ 侧 `Preset inode -> '{}'`） |
