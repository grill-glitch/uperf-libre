# ⑥ 热反馈调预算（fas-rs 的 `core_temp_thresh`，先削 PL1 不先削频点）

## 做法

fas-rs 用 `core_temp_thresh` 在过热时调整**目标帧率**；本项目的对应杠杆是 CPU 调频器的
**持续功耗预算 PL1**（`GovernorTunables.slow_limit_power`）——**温度永远不参与挑频点**。

```
温度 ──> ThermalPolicy::pl1_scale ──> Governor.thermal_scale ──> pl1 = slow_limit_power * scale
                                     （PL2 / OPP 目标一律不动）
```

* `pl1_scale(t, p)`：`t <= thresh_c` 时为 `1.0`；到 `thresh_c + span_c` 线性降到 `floor`，
  再低就夹在 `floor`（默认 60 °C / 20 °C / x0.5，均可用 env 覆盖）。
* 削 PL1 在两处生效：短时能量池按（变小后的）PL1 加速耗尽；`guideCap`/`limitEfficiency`
  之后的硬功率上限用的也是同一个值——所以只有**预算**变了，频点仍从各 cluster 自己的
  OPP 表里选。
* 温度取自 `/sys/class/thermal` 里 `type` 含 `cpu-` 的 zone（alioth 上 `cpu-N-N-usr` 与
  `cpu-N-N-step` 都是实硅传感器），取**最热的那个**；1 Hz 采样（调频器 tick 远快于任何
  zone 变化）。读不到 zone 就不动 PL1，绝不臆造温度。

env：`UPERF_THERMAL_THRESH_C`、`UPERF_THERMAL_SPAN_C`、`UPERF_THERMAL_FLOOR`、
`UPERF_THERMAL_ROOT`（默认 `/sys/class/thermal`）。

## 验证

[V] `cargo test --release`：`uperf-config` 新增 5 个 thermal 单测 + 1 个 governor 单测
（`a_thermal_scale_eases_pl1_without_inventing_a_frequency_point`：热的一方**不得**高于冷
的一方、必须真的绑住、且每个目标频点**都在该 cluster 自己的 OPP 表内**）。

[V] 真机 e2e（`tools/binder-probe/e2e-thermal.sh`，alioth，fake root + `UPERF_CPU_GOVERNOR=1`）：

```
--- thresh=25（同一台设备此刻算"热"）---
Rust: thermal 24 zone(s) under /sys/class/thermal, now 33.3 C (thresh 25 C, floor x0.50)
Rust: thermal 33.3 C -> PL1 x0.79 (of 1.00 W)
Rust: thermal 31.8 C -> PL1 x0.83 (of 1.00 W)
--- thresh=90（同一温度算"冷"）---
Rust: thermal 24 zone(s) under /sys/class/thermal, now 32.5 C (thresh 90 C, floor x0.50)
        （没有降 PL1 的行 —— 预算未被削）
真实 governor（对照）：policy0=schedutil  policy7=schedutil   ← 全程未被碰
```

## [U] / 未做

* 阈值/跨度/地板是**默认值**，不是从上游或校准得来的；不同 SoC 需要各自标定。
* 只在 governor 接管（`UPERF_CPU_GOVERNOR=1`，默认关）时才可能生效。
* 没做：与帧率/场景联动（fas-rs 是"降目标帧率"，本实现是"降功耗预算"，两者不等价，
  是有意的选择）；也没做温度迟滞（hysteresis），故在阈值附近可能来回抖动。
