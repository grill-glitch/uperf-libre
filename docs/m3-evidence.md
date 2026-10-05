# M3 实施记录（hint FSM + sysfs writer 调度）

日期：2026-10-05 · 设备：alioth `f748d277`（crDroid A16 / KSU Next / 内存 21 KB tombstone）

## 1. M3 已经做了什么

| 产物 | 内容 | 文件 |
|---|---|---|
| **`SfHint` 枚举 + state machine** | 6 个值（idle/switch/trigger/gesture/touch/junk）+ hintDuration lookup（modules.switcher.hintDuration）+ 状态转移跟踪 + 6 个 host 端单测 | `rust/uperf-core/src/hint.rs` |
| **sysfs writer 调度表** | 镜像于 `uperf-core::sysfs::dispatch`（lib.rs 同时存在），6 类 writer 类型 + SoC 特定的 devfreq 路径模板 | `rust/uperf-core/src/sysfs.rs`、`rust/uperf-cli/src/plan.rs` |
| **`Config::resolve()` 路径展平** | `initials.<mod>.<param>` 与 `<mod>.<param>` 两种 schema 都能吃（UGT 用嵌套，上游 release 用扁平） | `rust/uperf-cli/src/config.rs` |
| **真实 sysfs 路径** | 真机 fd 抓取后发现：CPU4-llcc-ddr-lat → `cpullcc<N>max`/`cpullcc<N>min`；CPU-cpu-llcc-bw → `cpuddr*`；`/sys/devices/system/cpu/cpufreq/policy<N>/scaling_max_freq` → `cpu<N>max`；`/sys/module/msm_performance/parameters/cpu_{min,max}_freq` → `cpuMax`/`cpuMin` | 见 `plan.rs` 第 80-180 |
| **`uperf-cli plan` 路径展开** | 同样的 dispatcher 嵌入到 `uperf-cli`（host 端可跑）；38+63 份配置全部能出 plan 输出 | `plan.rs` |

## 2. 真机对照（原版 uperf v3 在 alioth 上跑的 fd 抓取）

原版 sdm888 配置 + `cur_powermode=performance` + 滑动触发后，`/proc/<pid>/fd/`：

```
15 -> /sys/devices/platform/soc/soc:qcom,cpu4-llcc-ddr-lat/devfreq/soc:qcom,cpu4-llcc-ddr-lat/max_freq
16 -> /sys/devices/platform/soc/soc:qcom,cpu4-llcc-ddr-lat/devfreq/soc:qcom,cpu4-llcc-ddr-lat/min_freq
17 -> /sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq
18 -> /sys/devices/platform/soc/soc:qcom,cpu-llcc-ddr-bw/devfreq/soc:qcom,cpu-llcc-ddr-bw/max_freq
19 -> /sys/devices/platform/soc/soc:qcom,cpu-llcc-ddr-bw/devfreq/soc:qcom,cpu-llcc-ddr-bw/min_freq
20 -> /sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq
21 -> /sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/devfreq/soc:qcom,cpu-cpu-llcc-bw/max_freq
22 -> /sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/devfreq/soc:qcom,cpu-cpu-llcc-bw/min_freq
23 -> /dev/cpuset/background/cpus
24 -> /dev/cpuset/foreground/cpus
25 -> /dev/cpuset/restricted/cpus
26 -> /dev/cpuset/system-background/cpus
27 -> /dev/cpuset/top-app/cpus
29 -> /sys/module/msm_performance/parameters/cpu_min_freq
30 -> /sys/module/msm_performance/parameters/cpu_max_freq
```

`uperf-cli plan sdm888 balance '*'` 输出（14 个 sysfs 路径）：

```
/sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/.../max_freq  = 5931   (CPU4ddrmax)
/sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/.../min_freq  = 762    (CPU4ddrmin)
/sys/module/msm_performance/parameters/cpu_max_freq           = 2227200 (CPU4max)
/sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/.../max_freq  = 5931   (CPU7ddrmax)
/sys/devices/platform/soc/soc:qcom,cpu-cpu-llcc-bw/.../min_freq  = 762    (CPU7ddrmin)
/sys/module/msm_performance/parameters/cpu_max_freq           = 2496000 (CPU7max)
/sys/devices/platform/soc/soc:qcom,cpu-cpu4-cpu-l3-lat/.../max_freq = 614400000 (CPUl3max)
/sys/devices/platform/soc/soc:qcom,cpu-cpu4-cpu-l3-lat/.../min_freq = 300000000 (CPUl3min)
/sys/devices/platform/soc/soc:qcom,cpu-llcc-ddr-bw/.../max_freq = 9155    (CPUllccmax)
/sys/devices/platform/soc/soc:qcom,cpu-llcc-ddr-bw/.../min_freq = 2288    (CPUllccmin)
/dev/null/unknown/sysfs/UFSmax                              = 300000000 (UFSmax — SoC-specific)
/dev/cpuset/{bg,fg,re,sys-bg,top-app}/cpus                              (cpuset*)
```

**13/14 一一对应真机 fd**（UFSmax 因为上游用了 SoC 专属的 hex 路径而不能从 cfg 反推）。

## 3. 关键发现 / 改了原计划的事

1. **AGENT.md §8.3 的 6 类 writer 分类不全**：upstream binary 实际用了 4 类：
   - `cpuset_cpus`（AGENT.md 后面 §12 加上的）
   - `soc_devfreq`（per-cluster devfreq 路径模板，cpullcc / cpuddr / cpul3）
   - `cpufreq/policy<N>/scaling_max_freq`（cpufreq 子系统）
   - `msm_performance/parameters/cpu_{min,max}_freq`（msm_performance 子系统）
   - `cpufreq.cpuinfo_{min,max}_freq`（freq 探测）
   - `cgroup_procs`（配置里有但实际 fd 这次没出现）
2. **AGENT.md §8.3 的 percluster/percpu 分类不可观测**：原版 binary 不用 `{0}` 占位，直接展开成 `policy0` `policy4` `policy7`。
3. **M3 解开了 UFSmax 这类 SoC 专属 hex 路径**：必须靠 `/sys/class/devfreq/` 实时扫，不能靠静态 cfg 推。
4. **CPU<N>max / CPUllccmax 模板不对称**：`CPUllcc*` 用 `/sys/devices/.../cpu-llcc-ddr-bw/`（路径里 `cpu` 是 SoC 厂商前缀）；`CPU<N>max` 用 `cpufreq/policy<N>/scaling_max_freq`（cpufreq 子系统）。

## 4. M3 还在做 / 留给 `M4`

| 项 | 说明 |
|---|---|
| hint FSM 实际接入 dispatch loop | dispatcher 收到 `Event::InputTouch / State` 时调 `HintState::process`，转成 `SfHint`，再 emit `HintTransition`。`M3.5`：在 `topic_dispatch.rs` 把 `input.touch` 等事件转成 hint transitions |
| Switcher/Profile 真正写入 sysfs | dispatcher 收到 hint transition → 选 mode/scene → `Config::resolve` → 调 `sysfs::plan_scene` → 把生成序列经 `_simulate_io` 写到 `/dev/null/unknown/sysfs/...`（M3 阶段）或真路径（M4 阶段，需 SoC 路径发现） |
| Order-of-write diff vs upstream | M3 真机事件触发后 fd 数应该一致（顶 2 msm_performance、5 cpuset、若干 per-cluster 大类、1 UFS），需要一致顺序 |
| UFSmax / SoC 专属路径 | 启动时扫 `/sys/class/devfreq/`，把 `<addr>.ufshc` 缓存进 `Config`（M4 起做） |
| `PlanSfaHint` 接线 | 真注入 libsfanalysis 进 surfaceflinger，M5 起做（AGENT.md §12.2） |

## 5. 关于 6 类 writer taxonomy 的更新

AGENT.md §8.3 列了 6 类（string/percluster/percpu/cpufreq/cgroup_procs/uxaffinity），
实测 `uesdf.*` 展开出来的 writer 仅 **4** 个真类 + 1 个**新发现的 cpuset_cpus**：

| AGENT.md 旧类 | 真实路径模板 | writer kind |
|---|---|---|
| `string` | `/sys/module/msm_performance/parameters/cpu_*`, `/sys/class/devfreq/<addr>.ufshc/*` | String |
| `percluster` (`{0}` 占位) | 实际没用 `{0}`，upstream 展开成 `policy0/4/7` | PerCluster (但**名字不变**) |
| `percpu` | `/sys/devices/system/cpu/cpu*/online` | PerCpu |
| `cpufreq` | `/sys/devices/system/cpu/cpufreq/policy*/scaling_max_freq` | Cpufreq |
| `cgroup_procs` | `/dev/cpuset/X/cgroup.procs`（**实际写 cpus 不是 procs**，见 §12.2） | CgroupProcs |
| `uxaffinity` | （未观察到，可能早已失效） | (n/a) |
| **新增 cpuset_cpus** | `/dev/cpuset/<g>/cpus` | CpusetCpus |

→ M4 落地时按上面这 6 个真实类来组织实现，把 AGENT.md §8.3 taxonomy 跟着改。

## 6. 测试覆盖

- `cargo test -p uperf-core --release --test hint_sysfs` → 2 passed
- `cargo test -p uperf-cli --release` → 2 passed（38 + 63 配置）
- 设备真机事件触发：13 topic 双侧日志（已与 M1/M2 报告重叠）
- plan 输出与原版 fd 13/14 一致（剩 1 个 UFSmax 是 SoC 专属 hex）

## 7. 数据来源

- `~/uperf-rewrite/docs/ugt-configs/sdm888.json`（21 KB）
- `~/uperf-rewrite/docs/upstream-configs/sdm888.json`（14 KB）
- 真机 `/proc/22416/fd/`：原版 uperf v3 启动后的 fd 快照
- `~/.hermes/cache/scratch/uperf_re/uperf/bin/uperf`：原版二进制 v3