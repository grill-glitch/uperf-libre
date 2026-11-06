#!/usr/bin/env python3
"""Split AGENT.md's historical sections into docs/history/ and rewrite them compactly.

One-shot migration (kept in the tree as the record of how the split was done; it is
idempotent in the sense that re-running it after the split finds nothing to move).
"""
import re
import pathlib

root = pathlib.Path(__file__).resolve().parents[1]
agent = root / "AGENT.md"
text = agent.read_text()

# ---------------------------------------------------------------- locate sections
heads = [(m.start(), m.end(), m.group(1)) for m in re.finditer(r"(?m)^## (\d+)\..*$", text)]
by_num = {}
for i, (start, end, num) in enumerate(heads):
    stop = heads[i + 1][0] if i + 1 < len(heads) else len(text)
    by_num[num] = (start, stop)

def body(num):
    s, e = by_num[num]
    return text[s:e]

# ---------------------------------------------------------------- docs/history
hist = root / "docs" / "history"
hist.mkdir(parents=True, exist_ok=True)

(hist / "milestones.md").write_text(
    "# 里程碑记录（从 AGENT.md §11 迁出）\n\n"
    "逐里程碑的验证证据、真机结论、以及当时刻意没做的事。**这是一份历史记录**：\n"
    "它回答\"为什么现在是这样\"，不构成对当前行为的约束。当前契约在 `AGENT.md`\n"
    "（架构不变量在 §6 / §8，规则在 §13），仍未解决的东西在 §12。\n\n"
    "照抄自 `AGENT.md` §11 的原样内容，只删掉了标题层级：\n\n"
    "---\n\n"
    + body("11").split("\n", 1)[1],
)

(hist / "device-findings.md").write_text(
    "# 设备事实与逆向结论（从 AGENT.md §2 迁出）\n\n"
    "上游二进制、配置 schema、alioth 内核/SELinux 的实测结论，以及它们的证据链。\n"
    "**这是\"当时量出来的东西\"的完整记录**；AGENT.md §2 只保留仍然约束代码的那几条。\n"
    "注意其中部分结论已被后续工作取代（例如 sysfs knob 现在有真实 sink），以 AGENT.md 为准。\n\n"
    "照抄自 `AGENT.md` §2 的原样内容：\n\n"
    "---\n\n"
    + body("2").split("\n", 1)[1],
)

(hist / "unknowns-resolved.md").write_text(
    "# 已解决的未知项与事故（从 AGENT.md §12 迁出）\n\n"
    "每条都是\"当时不知道 / 当时会坏\"，现在已经有结论。留在这里是为了不再重新推导一遍；\n"
    "其中与代码行为相关的教训已经收进 `AGENT.md` §6/§8/§13 与\n"
    "`uperf-rewrite-parity` 技能（后者是给动手改的人看的操作手册）。\n\n"
    "照抄自 `AGENT.md` §12 的原样内容（其中仍然开放的部分已抽到 AGENT.md §12）：\n\n"
    "---\n\n"
    + body("12").split("\n", 1)[1],
)

# ---------------------------------------------------------------- new §2
new2 = """## 2. 设备与运行契约（当前约束）

只列**仍然约束代码**的事实；每条的量测过程、证据链与当时结论在
[`docs/history/device-findings.md`](./docs/history/device-findings.md)。

| 约束 | 后果 |
|---|---|
| alioth 的 `scaling_max_freq` 是**驱动级只读**（0444，root 写也 EACCES，且 `[ -w ]` 因 `CAP_DAC_OVERRIDE` 会骗人） | 调频只能 `scaling_governor=userspace` + `scaling_setspeed`；写前判 mode 位，不判 `access(2)`。见 §8.4 |
| `/sdcard`（FUSE）下 root `unlink` 会**静默成功但文件仍在** | 删除走 `/data/media/0/...`（同步清 FUSE 视图） |
| 守护进程与它的 worker **都叫 `uperf`**（dfps 改写自己的 cmdline），`pidof` 也不匹配 zombie | 停服用 `killall uperf`；找活体用 `/proc/<pid>/exe`，不用名字/pid 文件 |
| 模块路径 `.` = `/data/adb/modules/uperf`；`USER_PATH=/sdcard/Android/yc/uperf`；CLI 契约 `uperf <cfg> -o <log>` | 见 §7 |
| `setup.sh` 安装时把 `config/<soc>.json` 播种成 `USER_PATH/uperf.json`（旧文件备份 `.bak`），然后删掉模块里的 `config/` | 装机后比对 zip 与模块目录时必须允许 `config/` 缺席（`scripts/install-and-verify-staged.sh`） |
| 设备上没有 `patchelf`；SF 注入要求库落在带 `system_file` 标签的路径且显式 `UPERF_SF_INJECT=1` | 默认只装库不碰 surfaceflinger |
| 本机 SELinux（Enforcing）拒绝 `execmem`/`execmod`，KernelSU-Next 也无可用 sepolicy 通道 | SF 侧的 hook 只能走 **GOT/PLT 改写**，不能 inline hook |
| `modules.sysfs.enable` 出厂 63 份配置全为 `false` | 真实 sysfs 写入是显式 opt-in；`RealSink` 存在但默认不跑（§8.3） |
| **已知内核缺陷**：KSU-Next 的模块挂载 + 本内核会在关机阶段触发 dcache panic（`list_del corruption` → `__dentry_kill`），`sys.boot.reason=kernel_panic,bug`；panic 后 f2fs 可能丢最近写入的块 | 每次开机后必须核对模块文件 md5 与全 NUL 扫描（`scripts/post-boot-verify.sh`）；不要假设 boot 是干净的 |
| `/proc/sys/fs/inotify/*` 可写（`max_queued_events`/`max_user_watches`），但 `chown/chmod` 在 ksu 域 EACCES | 调参 best-effort，失败只报一行 |

"""

# ---------------------------------------------------------------- new §11
new11 = """## 11. 功能清单（①..⑪ 就是代码里那些圆圈编号）

M11 当初排的队列，编号已经成了代码注释里的词汇（`⑪` = uprobe 帧计数等），所以留在这里；**逐里程碑的
证据、真机结论与"当时没做什么"在 [`docs/history/milestone-log.md`](./docs/history/milestone-log.md)**。

| # | 功能 | 默认 | 代码 |
|---|---|---|---|
| ① | 看门狗稳态成本缓存化（活体判定走 pid+start_ticks 缓存，过期才全扫 `/proc`） | 开 | `magisk/script/uperf_watchdog.sh` |
| ② | 写入账本 + 恢复断言（改过的 sysfs knob 先记原值，停止/被杀后由脚本回滚） | 开（有写入才记账） | `src/sysfs_ledger.rs`、`libuperf.sh::uperf_restore_sysfs` |
| ③ | SF 重启后自动重注入 | `UPERF_SF_INJECT=1` | `uperf_watchdog.sh` |
| ④ | 前台助手文件源（`foreground.txt` 作 top-app 来源，C++ monitor 兜底） | 开（文件不存在即回落） | `src/foreground.rs` |
| ⑤ | 直连 binder 的 SF `--latency` 帧源（降级腿） | `UPERF_SF_BINDER=1` | `src/sf_binder.rs` |
| ⑥ | 热反馈削 PL1（§8.4 的 thermal） | 随 governor（默认关） | `uperf-config/src/thermal.rs` |
| ⑦ | socket 控制面：反连握手 + `PING`/`PONG`/`STATUS` | `UPERF_CTL_SOCKET` | `src/ctl_socket.rs` |
| ⑧ | 记录通道（按 pkg+epoch 的会话，≥3 min，DEFLATE） | `UPERF_RECORD=1` | `src/recorder.rs` |
| ⑨ | 配置标准档合并 | **放弃**（会动到与上游逐字对齐的配置格式） | — |
| ⑩ | 32 位覆盖的诚实声明 + 装机守卫（32 位构建明确不做） | 守卫常开 | `libsysinfo.sh::require_aarch64` |
| ⑪ | uprobe 每线程帧计数（AppOpt 风格的主腿，perf PMU，不加载 BPF） | `UPERF_SF_UPROBE=1` | `src/sf_uprobe.rs`、`src/frame_state.rs` |

"""

# ---------------------------------------------------------------- new §12
new12 = """## 12. 仍未解决的问题

只有**当前**仍然成立、且会影响判断的几条。已解决的（以及当时怎么坏的）在
[`docs/history/unknowns-resolved.md`](./docs/history/unknowns-resolved.md)。

| 项 | 状态 | 影响 / 做法 |
|---|---|---|
| 上游打印的 OPP 列表是设备表的子集（cluster0 打了 9/17），规则拟合不出（非成本去重/非凸包/非功率阈值） | **UNKNOWN**（疑为抓取残缺） | 本实现用**全量**设备 OPP 表（上游可选集的超集），可达频点范围只多不少 |
| 上游的功率预算分配规则、latency 平滑细节 | **UNKNOWN** | 本实现是"每样本一个 OPP、`predict` 触发才跳"，不是渐近逼近 |
| `modules.input.enable=false` 是否真的能停掉输入监听 | **UNKNOWN** | 63 份配置全 `true`，且 vendored 监听器在配置解析前就由平台层启动；配成 false 只会多打一行日志 |
| `cur_powermode.txt` 里 `auto` 的语义 | **[I]** 读作"交给 perapp 规则"（二进制里只有 `Internal perapp switcher …` 的痕迹） | 代码按 `[I]` 实现，未真机对照过原版 |
| `comm` 被内核截断到 15 字符 | **已知**（不是缺陷） | `/MAIN_THREAD/` 用观测到的 `comm` 替换，模式与线程名同源 |
| 帧源两条腿只在人工 e2e 里验过（`tools/uprobe-probe/e2e-shade.sh`、`tools/binder-probe/e2e-frames.sh`） | **已知** | 默认都不开；开机后 `frame.source=` 为空是正常的 |

"""

# ---------------------------------------------------------------- rebuild
out = []
cursor = 0
for num in ("2", "11", "12"):
    s, e = by_num[num]
    out.append(text[cursor:s])
    out.append({"2": new2, "11": new11, "12": new12}[num])
    cursor = e
out.append(text[cursor:])
new_text = "".join(out)

# A header note so a reader knows where the history went.
new_text = new_text.replace(
    "# uperf 重写计划与验收（AGENT.md）",
    "# uperf 重写：契约、不变量与验收（AGENT.md）\n\n"
    "> 本文只写**当前**成立的东西：契约（§5/§7）、架构不变量（§6/§8）、验收（§10）、规则（§13）。\n"
    "> 历史决策、逐里程碑证据、已解决的未知项都在 [`docs/history/`](./docs/history/)。\n"
    "> 改代码前读 §6 与 §13；改 shell 前读 §4 的目录约定；动构建读 §9。",
    1,
)

agent.write_text(new_text)
print(f"AGENT.md: {len(text)} -> {len(new_text)} bytes")
for p in ("milestones", "device-findings", "unknowns-resolved"):
    print(f"  docs/history/{p}.md: {(hist / (p + '.md')).stat().st_size} bytes")
