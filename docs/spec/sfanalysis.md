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
**不引入 frida / strace / ltrace 依赖**。
