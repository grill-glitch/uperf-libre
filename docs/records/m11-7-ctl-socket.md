# ⑦ socket 控制面 —— 便宜的握手子集

## 形态（借 AppOpt，**反连**）

AppOpt 的 App 建一个本地 socket，Rust 守护进程把数据**推**过去，双方用
token/version/pid 握手。本项目照抄这个**方向**：**守护进程主动连出去**（反连），对端是
谁想拿守护进程的数据/想驱动它（控制器、工具，将来可能是场景 App）。

本轮只做**便宜的那一半**（队列原文"先做便宜的握手子集"）：

```
HELLO v=1 pid=<pid> uid=<uid> token=<32 hex>\n      ← 守护进程发出
OK v=1\n                                            ← 对端必须回；版本不同即拒
ERR ...\n                                           ← 对端拒 → 记日志并退避重连
PING\n / PONG\n                                     ← 之后的保活（守护进程驱动）
```

* **token**：`UPERF_CTL_TOKEN_FILE`，否则 `<USER_PATH>/uperf.token`；文件里有就用，没有就
  从 `/dev/urandom` 生成 16 字节（hex）并以 **0600** 落盘。取不到熵就不发握手（不发明 token）。
* **socket**：`UPERF_CTL_SOCKET`；`@name` = **抽象本地 socket**（Android `LocalSocket` 惯用
  法，无文件系统权限问题），否则按路径。留空 = 功能关闭（默认关）。
* **保活**：守护进程每 `UPERF_CTL_PING_MS`（默认 2 s）发 `PING`，连续 3 次没人回就判定链路
  断了并重连——不是"连着就一直算健康"。也回答对端发来的 `PING`。
* **命令集**：还没有。半成品命令集需要一个真实消费者来设计，本轮故意不做。

## 验证

[V] 主机 `cargo test --release`：`uperf-core` **133**（+6 ctl_socket：HELLO 往返、缺字段/空
token 拒、回复严格解析（`OK` 无版本即错）、token 随机 32 hex 且 0600 且二次读回一致、抽象与
路径两种地址、**握手对接自建 listener**（相同版本收、`OK v=2` 与 `ERR` 拒））。

[V] 真机 e2e（`tools/ctl-listen/e2e-ctl.sh`，alioth；peer = 本仓 `tools/ctl-listen`）：

```
--- peer [accept] ---
ctl-listen: bound @uperf-e2e (reject=false, 14s)
peercred: uid=0 pid=21312
hello: HELLO v=1 pid=21312 uid=0 token=441f19ea910ba854ac2297f2e8018bb0
hello_well_formed=true
replied: OK v=1
pong
--- daemon [accept] ---
Rust: ctl-socket: handshake ok, peer v1
Rust: ctl-socket: pong #1
--- peer [reject] ---
replied: ERR refused by ctl-listen
--- daemon [reject] ---
Rust: ctl-socket: handshake failed: peer refused the handshake      （并按 2 s 退避重试）
token file: -rw------- 33 bytes
```

即：反连、token/version/pid 握手、双向保活、拒连路径，全部真机跑通；token 文件权限 0600。

## [U] / 未做

* **没有命令集**（`status`/`set-preset`… 仍走 `webui.sh` 的 shell 面）；socket 面目前只是
  一条被验证过的握手通道。
* 对端的 **token 校验**在对端侧，本仓的验证工具只检查形状（`v=1` + 有 token），不核对值。
* 没有对端 uid 白名单（`SO_PEERCRED` 已在工具侧打印，但没拿它做准入）。
* SELinux：本机双方同为 root/ksu 域，抽象 socket 连接未被拦；换域（如 App 侧）需另测。
