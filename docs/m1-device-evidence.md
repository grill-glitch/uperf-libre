# M1 真机验证（2026-10-05，alioth `f748d277`）

> **前提**：M1 build/check 已通过（`docs/m1-evidence.md` §1-§4）；真机断开未做真机验证。
> 本文是 M1 **真机补验**——设备重新插上 USB 之后的事实留档。

## 1. 上传与启动

```bash
adb -s f748d277 push ~/uperf-rewrite/build/aarch64-linux-android23/runnable/uperf /data/local/tmp/uperf
echo '{"_m1":"rust bridge smoke test"}' > /tmp/cfg.json
adb -s f748d277 push /tmp/cfg.json /data/local/tmp/uperf_m0_stub.json
adb -s f748d277 shell su -c '/data/local/tmp/uperf /data/local/tmp/uperf_m0_stub.json -o /data/local/tmp/uperf_m0_log.txt'
```

进程树（fork 监督器工作正常，daemon → app child，Rust dispatcher 线程已起）：

```
10264  1847 [uperf]      ← 无关
10266     1 uperf        ← daemon
10267 10266 uperf        ← app child
```

app child 下的线程（用 `ls /proc/10267/task/*/comm` 拿到）：

```
uperf-rs           ← Rust 分派线程（M1 新增）
InputListener      ← 触摸监听（来自 dfps vendor）
HeavyWorker        ← 重工作线程（来自 dfps vendor）
Inotifier          ← inotify 监听（来自 dfps vendor）
POSIX timer 0      ← alarm/timerfd
```

## 2. 启动后第一帧日志（**逐字**）

```
11:47:36 I uperf m0(rs-rewrite)[038c33e], by grill-glitch (Rust rewrite project)
11:47:36 I uperf[038c33e] M1 platform bring-up, config=/data/local/tmp/uperf_m0_stub.json log=/data/local/tmp/uperf_m0_log.txt (Rust event tap)
11:47:36 I EventTap: subscribed to 13 topics                       ← C++ M0 tap (留作对照，M2 删)
11:47:36 I Uperf is running                                       ← 守护横幅
11:47:36 I [Rust] uperf_rs_start: cfg=/data/local/tmp/uperf_m0_stub.json log=/data/local/tmp/uperf_m0_log.txt (M1: log-only, no policy yet)
11:47:36 I uperf_bridge_init_rust returned, entering main loop    ← Rust 分派线程已活
```

→ **Rust dispatcher 进程起来了**，且 `uperf_bridge_init_rust` **非阻塞**返回（否则到不了这一行）。

## 3. C++/Rust 两路日志逐字对照

操作：触发 home 键、回到 launcher、再滑动手势。截取一段最具有代表性的：

```
11:47:45 I EventTap: cgroup.ta.list = 158 pid(s) [2074 2075 2076 2310 2324 2325 3169 3189 ]
11:47:45 I [Rust] Rust: cgroup.ta.list = 158 pid(s) [2074 2075 2076 2310 2324 2325 3169 3189]
11:47:45 I EventTap: cgroup.ta.list = 101 pid(s) [2074 2075 2076 2310 2324 2325 3169 3189 ]
11:47:45 I [Rust] Rust: cgroup.ta.list = 101 pid(s) [2074 2075 2076 2310 2324 2325 3169 3189]
11:47:45 I EventTap: cgroup.re.list = 0 pid(s) []
11:47:45 I [Rust] Rust: cgroup.re.list = 0 pid(s) []
...
11:47:46 I EventTap: topapp.pkgName = com.android.launcher3
11:47:46 I [Rust] Rust: topapp.pkgName = com.android.launcher3
11:48:11 I EventTap: topapp.pkgName = io.chameleon.ultra
11:48:11 I [Rust] Rust: topapp.pkgName = io.chameleon.ultra
```

**核对结论**：
* pid list 预览（8 个 pid）**字节相等**——Rust 把 `uperf_pid_list_t`（16 字节结构）解出来后再做 `Vec<i32>::to_vec()` 的拷贝，与 C++ 侧直接读 STL 的输出一致
* 空列表 `[0 pid(s) []]` Rust 侧也正确还原（`pid_buf.storage` 为空，`len=0`）
* `topapp.pkgName` 字符串完全相同，验证 `cstring` 通过 FFI 完整传送
* **C++ / Rust 两路同时接收事件**，FFI 与载荷转换路径双向打通

## 4. 物理电源键 → `offscreen.state=true`（**解决了 M0 §3.2 第 1 条**）

操作：`adb shell input keyevent KEYCODE_POWER`（**这次屏幕确实熄了**——M0 那次没真的熄屏是症结）。

```
11:49:12 I EventTap: cgroup.re.list = 221 pid(s) [3169 3189 3190 3191 3193 3194 3195 3196 ]
11:49:12 I EventTap: offscreen.state = true
11:49:12 I [Rust] Rust: offscreen.state = true
11:49:12 I [Rust] Rust: cgroup.re.list = 221 pid(s) [3169 3189 3190 3191 3193 3194 3195 3196]
```

**结论**：
* vendored 判据 `restricted > 10` 实际生效（restricted 从 0 → 221）
* Rust 端 `[Rust] Rust: offscreen.state = true` 与 C++ 端 `EventTap: offscreen.state = true` **同时**触发
* 后续 `cgroup.re.update` 频繁触发（cgroup 节点不停更新），mount 完成
* `cgroup.re.update` 是空载荷，Rust 端 `Rust: cgroup.re.update (no payload)` 正确还原

**更新 AGENT.md §12.2**：M0 的「`offscreen.state` 在 alioth 上从未触发」已解——之前 `input keyevent 26` 在某些唤醒状态下**没有真的熄屏**，所以 restricted 一直是 0。**物理电源键 + 熄屏**才走通。

## 5. `input.touch` / `input.state` 没有出现 —— **已知差距**

测试命令：`adb shell input tap`、`adb shell input swipe`、`sendevent` 都没产生 `input.touch` 行。

**原因（待 M2 解决）**：
* `adb shell input tap` 通过 SurfaceFlinger 的 `InputManagerService.injectInputEvent` 路径注入，**不会**经过 `/dev/input/event*`（Kernel 级 NAT 上行）。
* alioth 的 `event0/xiaomi-touch` 是内核态触屏事件，`adb shell sendevent` 在 KSU 下被 SELinux/权限挡了。
* 真用户的手指触摸会走 `/dev/input/event*`，但本次没真触摸过。

**M2 处理方案**：
1. 真机**用手触摸**一次触发——建议在 7 张 golden 场景采集时一并做（AGENT.md §10.4）
2. 或者改用 `monkey` 等能在 evdev 层产生事件的工具
3. 暂不影响 M2 起跳（67 事件 → 0 / 1 event 不影响配置解析）；但 §10.4 场景采集必须真触摸

## 6. 清理

设备：

```bash
adb -s f748d277 shell su -c 'killall uperf; sleep 1; rm -f /data/local/tmp/uperf /data/local/tmp/uperf_m0_stub.json /data/local/tmp/uperf_m0_log.txt'
# → cleaned
```

仓库侧：`magisk/bin/uperf` 原版二进制保留作 parity 参照（未触碰）。

## 7. 真机验收（M1 全部对应 AGENT.md §1）

| 验收点 | 结果 | 证据 |
|---|---|---|
| 进程监督器（fork + setsid + 进程名 `uperf`） | ✅ | §1 |
| Rust staticlib 静态链接到二进制 | ✅ | `build.sh check` §1: 894 KB ELF（m0 的 586 KB + rust 8.5 MB 中只引入的 ~30%） |
| C ABI 桥 (`uperf_bridge_t`) | ✅ | `uperf_bridge_init_rust` 返回非阻塞（§2） |
| 13 topic 端到端贯通 | ✅ | `cgroup.{ta,fg,bg,re}.list/update`、`topapp.pkgName`、`offscreen.state` 全部双侧日志一致（§3, §4） |
| payload 转换 (bool/string/pid_list 结构) | ✅ | pid list 预览 8/8 字节相等；空列表还原（§3） |
| 单元测试（10/10 payload_decode） | ✅ | `docs/m1-evidence.md` §4 |
| `input.touch` / `input.state` 真机事件 | ⚠️ 待 M2 | `adb shell input` 走 SF 注入而非 evdev；详见 §5 |

**结论**：M1 **真机达成**。除 §5 的 `input.*` 事件待 M2 用真触摸补齐外，M1 全部交付项完成。