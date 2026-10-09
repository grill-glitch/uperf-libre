# SfAnalysis 注入重写 spec（M8 权威边界）

> 本 spec 是 AGENT.md §12.1 的展开。SfAnalysis 的 SF 注入库由 `libsfanalysis.so`
> （vendor 闭源、26 KB / 47 函数、静态链 xHook）改为 `libsfanalysis_rs.so`
> （本仓库自研 Rust 实现、cdylib）。本 spec 钉死**接口契约**与**实现约束**。

---

## 1. 范围与边界

**In scope**（M8 重写交付）：
- `magisk/bin/libsfanalysis_rs.so`（替换 vendor `libsfanalysis.so`）
- 注入 surfaceflinger 的机制（patchelf `--add-needed`）
- 写 `<config dir>/sfanalysis.hint` 的 byte 协议
- mprotect + inline patch 的 hook 实现（替代 xHook）
- 6 值 SfHint FSM（idle/switch/trigger/gesture/touch/junk，≥6 = unknown）

**Out of scope**（不许越界）：
- **不重写**消费端 `SfAnalysisListener`（`rust/uperf-core/src/watch_task.rs`
  + `hint.rs` 已实现，M6b 真机过）
- **不动** §7.4 hint 文件协议（路径 / 单字节 / 0..5 枚举）
- **不动** `cpp/dfps/**`（vendor dfps 零改动不变量）
- **不动** `magisk/bin/uperf`（M0 已 Rust 重写）

---

## 2. 接口契约（**字节兼容，必须不破**）

### 2.1 注入路径

```
magisk/customize.sh:
  patchelf --add-needed libsfanalysis_rs.so $MODPATH/system/bin/surfaceflinger
```

（vendor 件原本就注入 surfaceflinger；M8 替换库名，路径同。）

### 2.2 hint 文件协议

| 项 | 值 | 来源 |
|---|---|---|
| 路径 | `<USER_PATH>/sfanalysis.hint` | §7.4、AGENT.md §12.2 已锁定 |
| 单字节 | 0..5 = SfHint 枚举 | §1.3（m1-static-reverse.md） |
| 写入时机 | hook `xh_refresh_loop` 每次被调用 | vendor 行为静态已知 |
| 内容 | `0x00` 至 `0x05`，无分隔、无追加字节 | 同上 |

### 2.3 SfHint 枚举（**与 §1.3 完全一致**）

```
SfHint[0] = "idle"     (8 chars)
SfHint[1] = "switch"   (6)
SfHint[2] = "trigger"  (7)
SfHint[3] = "gesture"  (7)
SfHint[4] = "touch"    (5)
SfHint[5] = "junk"     (4)
SfHint[6+] = "unknown" (14 chars)
```

### 2.4 进程 anchor

```
/system/bin/surfaceflinger   ← 注入目标进程（patchelf 机制决定）
```

---

## 3. 实现约束

### 3.1 hook 目标

```
xh_refresh_loop   ← 在 libandroidfw.so 内
                    /system/lib64/libandroidfw.so   （主）
                    /system/lib64/libandroid.so      （备用）
```

### 3.2 hook 机制（**不依赖 xHook**）

```
1. 解析 /proc/self/maps 定位 libandroidfw.so 的 .text 段
2. mprotect 把 .text 改成 RWX
3. 在 .text 中搜索 xh_refresh_loop 的特征字节序列（r2 静态推导）
4. 写入 LDR x16, =hook_trampoline; BR x16 跳转序列（arm64）
5. 原函数地址保存在 .data，hook 内调用原函数后跑 FSM，再写 hint byte
```

### 3.3 状态码 → FSM 触发

vendor libsfanalysis 的 hint byte 写入条件（待 r2 静态反推）：
- 0 (idle)     : 进入 refresh 周期、但无新 frame
- 1 (switch)   : 切换 preset / 切后台 / 切前台
- 2 (trigger)  : 触发某种性能事件（如 profile 切换）
- 3 (gesture)  : 手势识别
- 4 (touch)    : 触摸事件
- 5 (junk)     : 丢弃 frame

→ **M8 验收**：真机抓 byte 序列，与 vendor 件在相同输入下的 byte 序列逐 byte 对账。

### 3.4 build / 编译

- toolchain: NDK r30（与 `magisk/bin/uperf` 同）
- crate: `rust/uperf-sfanalysis/`（cdylib）
- 编译产物: `magisk/bin/libsfanalysis_rs.so`（stripped, ~26 KB 量级）
- **不依赖** `regex` / `pcre2` / 任何 xHook（自写 mprotect + 跳转）
- 仅依赖 `libc`（`mprotect` / `memcpy` / `open` / `write`）

---

## 4. 兼容性缺口（**逐条列出，不假装不破**）

| 项 | 与 vendor 的偏差 | 处理 |
|---|---|---|
| hook 跳转位置精度 | vendor 用 xHook 的 hook table 间接寻址；自写用特征字节搜索，理论上有 false positive 风险 | r2 静态找唯一序列；M8 真机 byte 序列对账，偏差 <1% 才合格 |
| mprotect 时机 | vendor 在 dlopen 后首次调用时改 RWX；自写同样 | 同行为 |
| hint 写入的 fd | vendor open + write；自写同 | 同行为 |
| `xh_refresh_loop` 多次 hook 链 | 若 libandroidfw 已被其他 .so hook，本库追加一层 trampoline；不破坏已有 hook | 保留原函数地址，trampoline 内先调原函数，再跑 FSM |
| 没有 xHook 的 deregister 路径 | dlclose 时不清 hook；M0-M7 阶段 surfaceflinger 从不 dlclose，本行为等同 vendor | OK |

---

## 5. 验收（必须脚本化）

### 5.1 `build.sh make check` 新增断言

```bash
# 1. magisk/bin/libsfanalysis_rs.so 必须存在
[ -f magisk/bin/libsfanalysis_rs.so ] || abort "missing libsfanalysis_rs.so"

# 2. vendor libsfanalysis.so 不能在发布产物里
! [ -f magisk/bin/libsfanalysis.so ] || abort "vendor libsfanalysis.so still present"

# 3. surfaceflinger 必须被 patchelf 注入了新库
$ANDROID_TOOLCHAIN/bin/*-patchelf --print-needed \
    magisk/system/bin/surfaceflinger | grep -q libsfanalysis_rs.so \
    || abort "surfaceflinger not injected"

# 4. 新库的导出符号必须是 uperf 域（不允许导出 xHook 残留）
$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf \
    --dyn-syms magisk/bin/libsfanalysis_rs.so | grep -E 'xhook|xH_' \
    && abort "xHook leaked into libsfanalysis_rs" || true
```

### 5.2 真机 byte 对账

1. 在 vendor 件（baseline）和新件（replacement）下，分别录制同一段
   30s SF 渲染日志（包含切前台、切后台、滑动、点击各 ≥3 次）
2. 把 `sfanalysis.hint` 的 byte 序列逐字节 diff
3. 不一致的位置写入 `docs/m8-sfanalysis-reverse.md §3` 的 deviation table
4. **deviation > 0**：M8 不通过；≤0：进入 M9（vendor 件过渡期结束）

### 5.3 0 闭源 blob 不变量

```bash
# magisk/bin/ 下所有 .so 都必须能溯源到本仓库（无 vendor）
find magisk/bin -name '*.so' | while read so; do
    # 简化：vendor libsfanalysis.so 已被排除；其余 .so（libuperf_rs.so、
    # busybox 等）都已在 AGENT.md §6 表中列出归属
    case "$(basename $so)" in
        uperf|libuperf_rs.so|libsfanalysis_rs.so|busybox) ;;
        *) abort "unknown .so in magisk/bin: $so" ;;
    esac
done
```

---

## 6. 进度追踪

| 步骤 | 状态 | 文档 |
|---|---|---|
| 1. r2 静态逆向上游 libsfanalysis.so | 待办 | `docs/m8-sfanalysis-reverse.md` |
| 2. 写 `rust/uperf-sfanalysis/` 骨架（cdylib） | 待办 | — |
| 3. hook.rs（mprotect + 跳转） | 待办 | — |
| 4. fsm.rs（6 值 FSM） | 待办 | — |
| 5. sink.rs（open + write 单字节） | 待办 | — |
| 6. NDK r30 build 集成 | 待办 | `build.sh` 新增 `build_sfanalysis` |
| 7. `magisk/customize.sh` patchelf | 待办 | — |
| 8. 真机 byte 对账 | 待办 | `docs/m8-sfanalysis-reverse.md` §3 |
| 9. `build.sh make check` 新增断言 | 待办 | §5.1 |

---

## 7. 不做 Frida

AGENT.md §10.5 没要求 SfAnalysis 走动态验证；r2 静态 + 真机 byte 抓取覆盖。
**不引入 frida 依赖**（真机验证用 ADB + strace，strace 是设备自带 `/system/bin/strace`）。

---

## 8. M8 前提被推翻（2026-10-09 真机证据）

真机测试发现 **`sfanalysis.hint` 没有生产者**，证据见
`docs/m8-sfanalysis-reverse.md §6`（符号表 + harness strace + daemon strace 三证）。
因此：

* **`libsfanalysis.so` 不是 hint 生产者**，是一个纯进程内代码补丁库
  （hook `ioctl`/`epoll_wait`/`pthread_cond_wait`/`pthread_cond_timedwait`，
  在目标里补 16 个 hook 点）。
* daemon 的 `SfAnalysisListener` 在轮询一个**永远不存在**的文件
  （`openat(hint, O_RDONLY) = ENOENT`，实测）。
* 所以本文档 §2/§3 描述的"重写它、产出同样的 hint 字节"**没有可对齐的对象**：
  没有 byte 序列可比，因为上游也没有。

### 决定：B — 复刻进程内行为（2026-10-09）

用户裁定：**彻底去除闭源 blob，但复刻 blob 行为**。因此
`rust/uperf-sfanalysis` 重写为行为等价的进程内观察库：

* 注入方式不变（`patchelf --add-needed` 进 surfaceflinger）；
* ctor 把调用线程设为 `SCHED_FIFO` prio 3（同 vendor）；
* worker 线程名 `xh_refresh_loop`，延迟 60 s 后安装，然后周期重装（同 vendor）；
* inline hook 4 个 libc 函数，替换函数**先调原函数、保存返回值、喂观察者、返回原值**；
* `ioctl` 额外匹配 `BINDER_WRITE_READ`(0xc0306201) 并解 `binder_write_read`；
* **不写任何文件、不开任何 IPC**（与 vendor 一致）。

**已记录的偏差**（都是 spec 允许/必要的，不是遗漏）：

| 偏差 | 原因 |
|---|---|
| `dlsym` 代替手写 ELF dynsym 遍历 | 等价且短；见 §3 |
| maps 扫描过滤 `x` 权限 | vendor 不过滤 → 补到 ELF 头段（§0/§5），必须修 |
| 无 POSIX `timer_create`(SIGEV_THREAD_ID) | vendor 的 DelayedWork 定时器只驱动它自己的状态机，无外部可观测影响；不实现以免引入信号/线程复杂度 |
| 不 open surfaceflinger/libandroidfw/libandroid | 那是 vendor 解析 ELF 找符号用的；我们用 dlsym |
| 诊断经 `write`（`eprintln`） | vendor 完全静默；仅在错误/`UPERF_SFANALYSIS_DEBUG=1` 时输出，默认不影响行为 |

**真机验证**（alioth / crDroid A16，隔离 harness，未接 SF）：

```
superf-sfanalysis: hooked ioctl (entry 0x…318c, tramp 0x…0000)
uperf-sfanalysis: hooked pthread_cond_timedwait (entry 0x…796c, …)
uperf-sfanalysis: hooked pthread_cond_wait (entry 0x…78ec, …)
uperf-sfanalysis: hooked epoll_wait (entry 0x…4d40, …)   ← 跟到 __epoll_pwait
calls: ioctl=-1 ioctl2=-1 epoll=-1
  ioctl = 2   epoll_wait = 1   cond_wait = 0   cond_timedwait = 1
  binder_txns = 1   binder_writes = 1   installed = 1
exit=0
```

### SF 侧注入实测（2026-10-09，见 m8-sfanalysis-reverse.md §8）

用 `ssanalysis` 模块自带的 `injector`（ptrace + 远程 dlopen）把库注入**运行中的**
surfaceflinger，纯内存、无磁盘改动：

* 先把库放 `surfaceflinger_data_file` 目录并 `chcon u:object_r:system_file:s0`
  （`/data/local/tmp` 读不了；`surfaceflinger_data_file` 不可 `execute`）；
* 我们的库：`comm="xh_refresh_loop"` 出现（ctor 与 worker 都跑了），但
  **SELinux 拒绝 `execmem` ×4** —— 正好 4 个 trampoline 的匿名 RWX mmap；
  libc 仍是 `r-xp`。
* vendor 库：同样注入成功，SF 里出现它打开的 `fd → surfaceflinger /
  libandroidfw.so / libandroid.so`（`fcn.00004d58` 确实执行），但**无任何 avc、
  无补丁、无 worker 线程** —— 它在 SF 的 maps 里找不到 `libandroidfw.so`，
  目标库选择落空，静默 no-op。

**所以 enforcing 的 A16 上两者都装不上 hook**，只是卡点不同（vendor 卡在目标库
不存在；本仓库卡在 `execmem` 策略）。vendor 模块没发布任何 `sepolicy.rule`，
它自己也没解决这个问题 —— 该特性在 enforcing 的新 Android 上本来就是死的。

**可选 opt-in**（未实现，属主动增强而非复刻）：模块加一条
`allow surfaceflinger self:process execmem`，本仓库的库即可真正安装 hook。

**回滚**：注入是纯内存的，SF 重启即恢复。注意 `setprop ctl.restart surfaceflinger`
在本 ROM 上会经 `onrestart restart zygote` 级联成**整机重启**（实测 uptime 归零），
比预期重。
