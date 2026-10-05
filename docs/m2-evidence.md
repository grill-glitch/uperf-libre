# M2 实施记录（配置系统 + parity 工具）

日期：2026-10-05 · 设备：alioth `f748d277`（crDroid A16 / KSU Next）

## 1. 反推出的真实 schema（38 份配置全量验证）

> AGENT.md 早先写的 `presets.<mode>.base_hint.<scene>` 是**错的**（那是从文档推的，不是从数据）。
> 38 份 `*.json` 里 **0 份**使用 `base_hint`。

```jsonc
{
  "meta":     { "name": "sdm888/sdm888+[22.09.04]", "author": "yc@coolapk" },
  "modules":  { "switcher": {...}, "atrace": {...}, "sfanalysis": {...}, "sysfs": {...},
                "sched": {...}, "cpu": {...}, "anim": {...}, "input": {...}, "log": {...} },
  "initials": { "cpu": {...}, "sysfs": {...}, "sched": {...} },   // 每个模块一个对象
  "presets":  { "balance":  { "*": {...}, "idle": {...}, "touch": {...}, "trigger": {...},
                              "gesture": {...}, "switch": {...}, "junk": {...} },
                "fast":     { ... }, "performance": { ... }, "powersave": { ... } }
}
```

实测（sdm888.json）：
```
balance[*]       = {cpu.latencyTime:0.2, cpu.slowLimitPower:2.0, cpu.fastLimitPower:2.0,
                    cpu.fastLimitCapacity:16.0, cpu.margin:0.2}
balance[idle]    = {cpu.baseSampleTime:0.04, cpu.baseSlackTime:0.08, cpu.predictThd:0.3,
                    cpu.limitEfficiency:true, sched.scene:"idle"}
balance[touch]   = {cpu.baseSampleTime:0.04, sched.scene:"touch"}
balance[trigger] = {cpu.latencyTime:0.0, cpu.margin:0.4, sched.scene:"touch"}
balance[gesture] = {cpu.margin:0.6, sched.scene:"touch"}
balance[switch]  = {cpu.latencyTime:0.0, cpu.slowLimitPower:3.0, cpu.fastLimitPower:5.0,
                    cpu.fastLimitRecoverScale:0.1, cpu.margin:0.4, sched.scene:"boost"}
balance[junk]    = {cpu.burst:0.6, sched.scene:"touch"}
```

**层叠优先级**（`Config::resolve`）：
1. `presets[<mode>][<scene>][<dotted>]`
2. `presets[<mode>]["*"][<dotted>]`
3. `initials[<dotted>]`

`initials` 的真实结构是 `{"cpu": {...}, "sysfs": {...}, "sched": {...}}`（每模块一个对象），
但**层叠时按扁平点号键比较**（`cpu.margin`、`sysfs.xxx`），所以 Rust 侧 `Config.initials`
只存顶层键、`resolve()` 用 `mod.param` 查——M2 现有实现把顶层 key 当点号键存，
`plan` 输出里出现 `cpu = {12 entries}` 这种行。M3 要把它展平（见 §4）。

## 2. `uperf-cli`（新 crate，host 端）

```
$ uperf-cli parse <cfg.json>            # 结构摘要
$ uperf-cli warn  <cfg.json>            # 未知模块/键告警
$ uperf-cli plan  <cfg.json> <mode> <scene>   # 层叠后的键值 + 来源
```

实测（38 份全量）：

```
Total: 38  parse OK: 38  Bad: 0
Total: 38  warn clean: 38  with-warnings: 0
```

`warn` 零误报意味着：38 份配置的 `modules.*` 全是已知模块（switcher/atrace/sfanalysis/
sysfs/sched/cpu/anim/input/log），且 preset 下的 scene 名全部合法。

集成测试（`rust/uperf-cli/tests/all_configs.rs`）覆盖 38 份，断言 meta.name/author 非空、
preset 有 scene、`cpu.margin` 可解析；`cargo test -p uperf-cli --release` → **1 passed**。

上游配置已 vendor 到 `docs/upstream-configs/`（38 个 .json），供测试与 parity 使用。

## 3. 真机回归（修完 stale-staticlib 之后）

```
19:58:15 I uperf m0(rs-rewrite)[8799e26], by grill-glitch (Rust rewrite project)
19:58:15 I uperf[8799e26] M1 platform bring-up, config=... (Rust event tap)
19:58:15 I EventTap: subscribed to 13 topics
19:58:15 I Uperf is running
19:58:15 I uperf_rs_start: cfg=... (M1: log-only, no policy yet)
19:58:15 I uperf_bridge_init_rust returned, entering main loop
19:58:24 I EventTap: cgroup.re.list = 212 pid(s) [...]
19:58:24 I EventTap: offscreen.state = true
19:58:24 I Rust: offscreen.state = true
...
19:58:30 I Rust: input.state = hold:true swipe:true gesture:false     ← 真 swipe 被识别
19:58:31 I Rust: input.touch = true / input.state = hold:true ...
```

**13/13 topic 全部真机验证**，其中 `input.touch` / `input.state`（含 `swipe:true`）
补上了 M1 遗留的那一项 → M1 §5 的缺口关闭。

## 4. 崩溃根因（工程性坑，已修）

现象：
```
E uperf(pid=…) terminated unexpectedly, try to get tombstone
#00 __memcpy_aarch64_simd  #01 uperf_bridge_write_log  #02 uperf_core::log_msg
signal 11, fault addr 0x79, x1=0x79 x2=0x78
```

定位（未 strip 构建 + `llvm-nm` 映射 backtrace）：
* `uperf_bridge_write_log` 被以 **(msg, len)** 调用，而 C++ 期望 **(tag, msg, len)** →
  把 `len=0x79`(121) 当成了消息指针。
* 根因：Rust 侧 FFI 签名已改成 3 参，但 **`libuperf_core.a` 未重新编译**，链接进的是旧 2 参版本。
* 反汇编证据（cargo 重编前 log_msg 的调用点）：`mov x0, data_ptr; add x1, len+1; bl write_log`
  → 2 参；重编后：`mov x0, xzr; ldr x1, data_ptr; mov x2, len` → 3 参。

修法：`build.sh::make_uperf` 现在**先 `cargo build`（`build_rust`）再 cmake**，`.a` 不可能再陈旧。

## 5. M2 剩余（进 M3 前需要补）

| 项 | 说明 |
|---|---|
| `initials` 展平 | 现在 `Config.initials` 只存顶层键（`cpu`/`sysfs`/`sched`），要展平成 `cpu.margin` 形式，否则 `plan` 的 sysfs 序列出不来 |
| `plan` 的 sysfs 路径展开 | 需要 `modules.sysfs.knob` 的 6 类写入器语义（M3）才能把 `sysfs.*` 展开成真实 path=value 序列 |
| 告警文案逐条对账 | 现在 `warn` 是自造文案；要与原版 `CfgMgr:` 逐字对齐，需真机跑原版抓日志（原版启动日志已抓过一次，见 `m1-static-reverse.md` §2.1，但没有产生告警的样本配置） |
| `modules.switcher.hintDuration` 读取 | `plan` 现在还没读 `modules.switcher.*`（静态段） |