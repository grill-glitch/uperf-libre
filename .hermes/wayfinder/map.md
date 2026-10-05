# Wayfinder map — dfps-rs 重写

## Destination

把 dfps 完整用 Rust 重写、嵌入现有 uperf KernelSU 模块。验收：
1. **alioth 上切帧率路径可用 [V]** — sf 后门 (`service call SurfaceFlinger 1035`)
   实测拒绝（root + Enforcing），终点的切帧率路径退回到 **`settings put system
   peak_refresh_rate`（小米路径）** 或 vendor-specific 设置；接受此 fallback 后
   的兜底验收。
2. AGENT.md 有 dfps-rs 里程碑表（M-列 + V/I/U 状态标注）
3. WebUI 增加 "刷新率" 标签页，可从 KernelSU 管理器里切换 fps 规则

## Notes

- 工作目录 `~/uperf-rewrite`，fork `grill-glitch/uperf-rewrite`，分支 `game-turbo`
- 所有 Rust crate 命名沿用 `uperf-*` 前缀（`uperf-config`/`uperf-core`/`uperf-cli`）
- 装机模式：embedded（共用 `magisk/` 模块树、`USER_PATH=/sdcard/Android/yc/uperf`）
- 真机验证目标：alioth (`f748d277`)
- 引用资料：原 `cpp/dfps/` 即将被删除，本次重写后**仓库里不应该再出现 `cpp/dfps/`**

## Decisions so far

<!-- 一行/ticket：gist + 链接 -->

- **[T01: sf-backdoor-probe]** — 结论：**sf 后门在 alioth 不可用**（root + SELinux Enforcing
  下 `service call SurfaceFlinger 1035` 5/5 全部 `Operation not permitted`，未观察到
  `dumpsys display` 的 modeId 变化）。dfps-rs 的默认切帧率路径**必须 fallback 到
  `settings put system peak_refresh_rate`**（小米）或 vendor-specific 设置。T01 子代
  理仍在写最终 report（取 AVC detail），结论不变。
- **[T02: config-format]** — 结论：上游 dfps 是个**独立模块**，
  USER_PATH `/sdcard/Android/yc/dfps/{dfps.txt, dfps_log.txt, dfps_cur.txt}`。
  配置是 256 字节/行的文本：注释 `#`，tunable 前缀 `/`，规则 `<pkg> <idle> <active>`。
  特殊包名 `*`（万能规则）和 `-`（熄屏规则）。
  详：`/tmp/dfps-config-format.md`。
- **[T03: ipc-or-same-binary]** — 结论：**(c) dfps-rs 跑在同一 Rust 二进制里**。
  理由：(a) `sfanalysis.hint` 这条 notify-file 协议已经承载多字节但语义与帧率不同，
  复用会冲突；(b) Unix socket 对一个 userspace 模块是过度工程；(c) 现有
  `topic_dispatch.rs` 已能将 cgroup/input/offscreen 派发给多订阅者，dfps 只是再加
  一个 orchestrator 订阅者，订阅 4 个已有 topic（input.touch/input.btn/
  topapp.*/offscreen.state）。同进程也消除双进程 supervisor 的复杂度。

## Tickets

### 已决（依据研究结论）

- **[T01]** ✓ — sf 后门不可用（已决，影响 T06 默认路径）
- **[T02]** ✓ — 配置格式与 USER_PATH（已决，影响 T06/T07/T11）
- **[T03]** ✓ — 同二进制（已决，影响 T07）

### 未决（依赖上述 3 张）

- **[T04: data-structures]** — HashMap vs BTreeMap、special-pkg enum vs string match
  - 阻塞于：T02（拿到 schema 后才能定 token 形状）— **现已解锁**
- **[T05: switch-call-frequency]** — 去重策略、`force=true` 是否保留
  - 阻塞于：T02（读 force 调用者靠代码读 schema 时一起看）— **现已解锁**
- **[T06: notify-file-path]** — 复用 uperf USER_PATH 还是 `/data/dynamic_refresh_rate`
  - 阻塞于：T01（决定默认路径是 sf 后门还是 settings put 时一并决）— **现已解锁**
- **[T07: module-merge]** — 单二进制 vs 双二进制、CMakeLists 调整
  - 阻塞于：T03（IPC vs 同进程）— **现已解锁**
- **[T08: webui-tab]** — 第 4 个标签页
  - 不依赖其它票（只等 dfps-rs daemon 的 contract 决定调用什么文件）
- **[T09: build-wiring]** — Cargo workspace、build.sh 闸门
  - 阻塞于：T07（合并方式决定 build.sh 写法）

## Not yet specified

（Fog 文件在 `.wayfinder-fog.md`。新问题出现时记在那里。）

## Out of scope

- 制作独立的 dfps 模块 zip（"embedded" 排除此路）
- 重写 uperf 的 C++ 主进程（dfps 重写 ≠ uperf 重写）
- 在 dfps 里实现 uperf 才有的功能（CPU 调度、调度器、atrace、log.level 等）
- 修改 dfps 的上游历史（重写不保留任何 vendored cpp/dfps）
- 在其它设备上验证（仅 alioth）
