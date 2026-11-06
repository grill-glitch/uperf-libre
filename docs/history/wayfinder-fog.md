# dfps-rs 重写 — wayfinder 状态

> 本文件是 **charting 阶段**的初稿。**正式地图是
> `.hermes/wayfinder/map.md`**（索引） + `.hermes/wayfinder/ticket-T*.md`
> （每张票的细节）。下面保留一份"近期状态快照"作为下一会话的快速上手。

## 终点（见 map.md §Destination）

把 dfps 完整用 Rust 重写、嵌入 uperf KernelSU 模块。验收：
1. alioth 上切帧率路径可用 [V] —— sf 后门实测拒，退到 `settings put` 路径
2. AGENT.md 有 dfps-rs 里程碑表（M-列 + V/I/U 标注）
3. WebUI 增加 "刷新率" 标签页，可从 KernelSU 管理器里切换 fps 规则

## 已决（**详细取证在 `docs/research/`**）

| ticket | 结论 | 证据 |
|---|---|---|
| **T01** sf 后门探针 | `service call SurfaceFlinger 1035` 在 alioth 6/6 全失败 (`Operation not permitted`)，不是 SELinux/uid，是 SurfaceFlinger 自身拒绝 shell 路径 | `docs/research/sf-backdoor-probe-verdict.md`、原始 transcript 在 `/tmp/sf-backdoor-probe.log` |
| **T02** dfps 配置格式 | 配置 `/sdcard/Android/yc/dfps/dfps.txt`：注释 `#`、tunable 前缀 `/`、规则 `<pkg> <idle> <active>`、特殊包名 `*`（万能）和 `-`（熄屏）；notify file `/sdcard/Android/yc/dfps/dfps_cur.txt`，内容为裸整数 | `docs/research/dfps-config-format.md` (327 行) |
| **T03** IPC vs 同进程 | **(c) dfps-rs 跑在同一 Rust 二进制里** —— 现有 `topic_dispatch.rs` 已能将 cgroup/input/offscreen 派发给多订阅者，dfps 再加一个 orchestrator 订阅者即可。sfanalysis.hint 已占用，Unix socket 无必要 | 详见 map.md 已决段 |

## 未决（map.md §Tickets）

- T04 data-structures — 已解锁（拿到 T02 schema）
- T05 switch-call-frequency — 已解锁（force=true 由调用者决定，T02 给出了）
- T06 notify-file-path — 已解锁（默认走 settings put，T01 已排除 sf 后门）
- T07 module-merge — 已解锁（同进程已定）
- T08 webui-tab — 不依赖其它票
- T09 build-wiring — 阻塞 T07

## 下一个 frontier（按依赖序）

`T04 → T05 → T06 → T07 → T08 + T09`

T08/T09 在 T07 后才能动；T04-T06 互不依赖，可单张会话决。

## 已废 fog（不再 relevant）

> **已被 T01/T02/T03 决掉的项，保留以便将来审计**：
> F1 单一 vs 多二进制 ──→ T03 决：同进程
> F2 事件汇要不要扩 ──→ T03 决：复用 `topic_dispatch` 现有面
> F3 vendored 库删除 ──→ 即执行项（不再是决策）
> F5 sf 后门可用性 ──→ T01 决：不可用
> F6 notify 路径 ──→ T02 决：原路径 `/sdcard/Android/yc/dfps/dfps_cur.txt`
> F7 dynamic_fps 配置格式 ──→ T02 决
> F11 dfps USER_PATH ──→ 嵌入式路径下：复用 `/sdcard/Android/yc/uperf/`，不再独立

> **仍需要决的项**（不再是 fog，是 ticket）：
> F4 build.sh 的 build_webui ──→ 移交给 T09 票处理
> F8 rule 数据结构 ──→ 移交给 T04 票
> F9 force=true 语义 ──→ 移交给 T05 票
> F10 SwitchRefreshRate 去重 ──→ 移交给 T05 票
> F12 module.prop / webui 增量 ──→ 移交给 T08/T11 票
> F13 initsvc.sh / webui.sh 钩子 ──→ 移交给 T07 票
