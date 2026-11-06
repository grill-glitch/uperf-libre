# 设备事实与逆向结论（从 AGENT.md §2 迁出）

上游二进制、配置 schema、alioth 内核/SELinux 的实测结论，以及它们的证据链。
**这是"当时量出来的东西"的完整记录**；AGENT.md §2 只保留仍然约束代码的那几条。
注意其中部分结论已被后续工作取代（例如 sysfs knob 现在有真实 sink），以 AGENT.md 为准。

照抄自 `AGENT.md` §2 的原样内容：

---


| 事实 | 证据 |
|---|---|
| `magisk/bin/uperf` = 上游 `dev-22.09.04` 二进制 | sha256 `f1265757009ff0c85dd8587d9e7bfcf5e51d10d36fe5e1341688215ae1fb49d8`（与上游发行包一致） |
| 该二进制 1,461,512 B、aarch64 PIE、stripped、NDK r24、`.text` 987,320 B / 2,320 函数 | `readelf` / `.eh_frame_hdr` 二分表解析（自洽校验通过） |
| 只依赖 `libm/libdl/libc`，`BIND_NOW` + full RELRO | `readelf -d/-l` |
| 26 个自有类名（含 `CpufreqWriter` 全家族）、14 个源文件路径、全量日志串保留在二进制里 | RTTI 名 + `__FILE__` 串 + `strings` |
| 调用契约：`bin/uperf <USER_PATH>/uperf.json -o <USER_PATH>/uperf_log.txt`，`USER_PATH=/sdcard/Android/yc/uperf` | `magisk/script/libuperf.sh:38`；`Usage: uperf [-o log_file] config_file` 亦在二进制内 |
| 进程名必须是 `uperf`（`killall uperf` 停服）；启动后脚本会把它移入 background cpuset | `libuperf.sh: uperf_stop/uperf_start` |
| 配置由 `setup.sh` 从 `config/<soc>.json` 拷成 `USER_PATH/uperf.json` | `magisk/script/setup.sh:67` |
| 配置 schema v3 = `meta` / `modules` / `initials` / `presets`，**层叠覆盖用点号键**（`cpu.margin`、`sysfs.xxx`、`sched.scene`） | 仓库内 `config/README.md`（376 行，v3 权威规范） |
| Q: 根 `README.md` 里的 `powermodes/actions`、`knob type` 是 **v1/v2 旧文档**，与 v3 配置不符 | 对照 `config/template.json` 与 `config/sdm855.json` 实际键 |
| 事件总线 topic 与 dfps 逐字相同 | `cgroup.{ta,fg,bg,re}.{list,update}`、`input.{touch,btn,state}`、`offscreen.state`、`topapp.pkgName`；uperf 独有 `anim.running`、`sfanalysis.hint`、`config.profile` |
| uperf v3 的日志 pattern 就是 dfps 的 `%H:%M:%S %L %v`；`Usage:`/`Uperf is running`/守护横幅/`v3(22.09.04)`/作者串与二进制逐字一致 | 见 §7.3 的证据链 |
| 12 个业务模块源文件名在二进制里保真：`anim_watcher / atrace_switcher / cgroup_listener / context_scheduler / cpu_governor / input_listener / offscreen_monitor / profile_switcher / sfanalysis_listener / sysfs_writer / topapp_monitor / main` | `__FILE__` 串 |
| uperf 比 dfps 多出的能力层 | `context_scheduler`（PCRE2 正则规则引擎）、`cpu_governor` + `cpu_busy_reader`、`profile_switcher`、`sysfs_writer` + 12 个 `CpufreqWriter*`、`sfanalysis_listener`、`anim_watcher`、`log_level_switcher` |
| **alioth 内核 `4.19.325-cip131-st15-perf`（crDroid A16 / KernelSU-Next / Enforcing）上复核（2026-10-10，M9/M10 期间）** | |
| `scaling_max_freq` 仍是**驱动级只读**：`policy0` 上「写同值」与「写不同值」都被拒（`Permission denied`），mode 0444（模块自己的 `powercfg_once.sh` 会 `lock_val` 成 0444，但拦截来自驱动而非 mode） | `scripts/cpufreq-write-probe.sh`（可重跑）与原始输出 `docs/records/cpufreq-probe-alioth.txt`；判定必须看 shell 对重定向失败的**自身**报告（readback 区分不了「写成功」与「写的正是原值」——旧版探针曾因此误报） |
| `/proc/sys/fs/inotify/*` 的**写入可用**（`max_queued_events`/`max_user_watches`=1048576、`max_user_instances`=1024），但 `chown/chmod` 在 ksu 域下 EACCES | 同上探针；`lock_val` 已改为 best-effort 并对失败明确报一行 |
| 设备上**没有 patchelf**（`bin/`、`/system/bin`、PATH 都没有），所以安装时不会注入 surfaceflinger；且 M8 注入仍被 SELinux 挡（§12.1）⇒ 注入改为**显式 opt-in**（`UPERF_SF_INJECT=1`），默认只装库不碰 SF（避免 "NEEDED 装不上 → surfaceflinger 起不来" 的 UI 死法） | `magisk/customize.sh`；装机日志 |
| daemon 日志量约 0.3-2 MB/分钟（活动相关，与 `modules.log.level` 无关：`trace` 也几乎不增），所以上限必须按**大小**轮转 | `docs/records/m10-log-cap.md` 的实测 |
| **M8 SF 注入的交付方式（2026-10-10 复核，真机 Enforcing）**：库放 `system_file` 标签的路径（`/data/misc/surfaceflinger/`，0700 但 root 可写）→ `injector <目标完整 cmdline> <lib>` 即可 `dlopen` 成功；SF 里出现 `r-xp` 映射 + `xh_refresh_loop`/`sfh_stats_loop` 线程，**无任何 execmem/execmod 拒绝、不需要任何 sepolicy 改动** | `/data/local/tmp/injector`（用法 `[cmdline] [lib]`，按**完整 cmdline** 匹配，不是 comm）；本文件下方 §12.1 |
| 两条**不需要**的路线（别在这上面花时间）：① `debug.ld.app.<name>`/`debug.ld.all` 预载在 `ro.debuggable=0` 的用户版上不生效（linker64 里有这两个串，实测注入 `sh` 0 命中）；② SF 自身 `.dynamic` 只有 1 个 `DT_NULL`、`.dynstr` 只剩 2 字节余量 ⇒ 无法原地插 `DT_NEEDED`，要走 patchelf 式重排（设备上确实有可用的 aarch64 `patchelf`，928 KB，`/data/local/tmp/patchelf`） | `scripts/cpufreq-write-probe.sh` 同期的 `m8_*` 侦察输出；`readelf -SW/-dW /system/bin/surfaceflinger` |
| **`modules.sysfs` 的 knob 表在真机上根本不写**（2026-10-10 实测，两层门都关着）：① 出厂配置里 `"sysfs": {"enable": false}`（其余 5 个 module 都是 true）；② 代码里真写只在 `UPERF_FAKE_ROOT` 设置时走 `UnderRootSink`，没有 fake root 时走 `CollectingSink`（只计划、只记日志，不落盘）。所以「启动写 16 个 knob、停止只恢复 governor」这个说法**不成立**——今天之前我按 M4 文档这么说过一次，是错的 | e2e 里把 enable 翻成 true + 真 `UPERF_FAKE_ROOT` 跑真二进制：`grep -ciE "sysfs\|knob\|fake-write" daemon_log.txt` = **0**；切 `fast/performance/balance` 三种 preset 后仍是 0（说明 plan 只在事件/场景路径上产生，preset 单独切不出来）。真机上唯一的 sysfs 写是 governor 接管（`scaling_governor` + `scaling_setspeed`），那条本来就有记录+恢复 |：`Internal error: Oops - BUG`，`pc: __list_del_entry_valid` → `__dentry_kill`/`dentry_kill`/`dput`/`terminate_walk`/`path_openat`/`do_sys_open`/`__arm64_sys_openat`，即**在 `openat` 里踩到被写坏的 dentry 链表**；寄存器里出现过 ASCII `" corrupt"`（`x7=0x74707572726f6320`）；`Tainted: G S W`。踩雷线程是 `uperf-watch`（本模块的 watcher，4 Hz 开文件 ⇒ 最常踩到），**但这不是它的 bug**：损坏来自别处，且与本轮 M8 注入无关（SF 无新 tombstone、注入只让 SF 停了几十毫秒）。两次 panic 与本模块历史里「疑与 `ksud sepolicy patch` 相关」的两次极可能是同一个 dcache 问题 | `sys.boot.reason=kernel_panic,bug`；`/sys/fs/pstore/console-ramoops-0`（两次 dump 的 call trace 一致）；dmesg 里 `KP KernelPatch Version: d05` banner（设备装了 `KPatch-Next` 内核补丁框架，首要嫌疑） |

---

