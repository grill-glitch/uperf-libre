# docs/ — 这个目录里有什么

按**用途**分目录，不按时间堆在一起。找东西的顺序：先 `spec/`（要遵守什么），再 `records/`（当时怎么做的、
证据是什么），最后 `history/`（为什么变成现在这样）。

| 目录 | 内容 | 什么时候看 |
|---|---|---|
| [`spec/`](./spec/) | 契约与规格：`config-v3.md`（配置语义）、`sfanalysis.md`（SF 注入的边界/机制/缺口） | 改配置层或注入相关代码前 |
| [`records/`](./records/) | 逐里程碑的设计与证据（`m0`..`m11`）、设备跑批日志（`m9-*-alioth.txt`、`cpufreq-probe-alioth.txt`）、`webui.md` | 想知道"这个结论是怎么来的" |
| [`history/`](./history/) | 项目史：`milestone-log.md`（原 `AGENT.md` §11 的里程碑表）、`device-findings.md`（设备/逆向事实全表）、`unknowns-resolved.md`（已解决的未知与事故）、`wayfinder-fog.md`（早期规划残留） | 想知道"为什么不再那样做" |
| [`audit/`](./audit/) | `unsafe-audit-report.md`：全仓 `unsafe` 审计（含为什么 Miri 用不了） | 动 `unsafe` 前 |
| [`research/`](./research/) | 外部调查：dfps 配置格式、SF 后门探针结论等 | 要对照上游行为时 |
| [`configs/`](./configs/) | 只读快照：`upstream/`（上游 v3 的 38 份）、`ugt/`（UGT 的 63 份）。发货用的是仓库根的 `config/` | parity 对账时 |

规则：

* **不新增顶层文档**。写新的里程碑记录 → `records/`；写新的契约 → `spec/`；写"我们试过 X 失败了" → `history/`。
* 移动或改名文档时，跑一遍 `scripts/tidy-docs.py` 之类的引用重写，并确认 `docs/` 下的路径引用全部存在
  （仓库里所有 `docs/...` 的引用都会被这个检查覆盖）。
* `records/` 里的历史结论可能已被后续工作取代（例如 sysfs knob 现在有真实 sink）；**当前契约以 `AGENT.md` 为准**。
