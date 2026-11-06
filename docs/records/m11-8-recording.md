# ⑧ 记录 / 自测规则与通道

四条规则（借 AppOpt 的 history 库），**规则本身才是这一项的重点**：

| 规则 | 实现 |
|---|---|
| **opt-in** | 只有 `UPERF_RECORD=1` 才记；关着时 `observe()` 立即返回 `None`，一个字节都不写 |
| **会话 ≥ 3 min** | `MIN_SESSION_MS = 180_000`（`UPERF_RECORD_MIN_MS` 可覆盖）：前台不足 3 分钟的一段直接丢掉，不平均、不凑数 |
| **Deflate** | 存储是**裸 DEFLATE**（无 zlib/gzip 包装），`flate2` 的 `rust_backend`（纯 Rust，交叉编译不牵 C 工具链） |
| **按 pkg+epoch 去重** | key = `sanitize(pkg)-<epoch_ms>`，既是**文件名**也是去重键；启动时扫 `history/` 把已有 key 读进来，同一 pkg+epoch 永不写第二遍 |

一条会话 = 一个包的一段前台：`{pkg, epoch_ms, duration_ms, samples[{t_ms, scene, fps}]}`。
fps 取 `sf_binder::last_fps()`（帧腿最后测到的值；帧腿没跑时是 0.0 —— 记的是"测到的事实"，
不是猜的值）。包切走、前台未知、以及 daemon 停止，都会关闭当前会话。

env：`UPERF_RECORD`、`UPERF_RECORD_MIN_MS`、`UPERF_RECORD_SAMPLE_MS`（默认 1000）、
`UPERF_RECORD_TICK_MS`。存储落在 `<USER_PATH>/history/`。

**为什么 daemon 是唯一的读者**：shell 面（`webui.sh`）inflate 不了 Deflate，所以这个库是
守护进程的私有历史；对外暴露（WebUI/⑦ socket）是有意的下一步，不是漏做。

## 验证

[V] `cargo test --release`：`uperf-core` **141**（+8 recorder：关着不记 / 短会话丢且长会话留 /
前台切走即关 / 采样受 cadence 限制且 t 从 epoch 起算 / **deflate 往返且真的压缩、首字节不是
zlib 的 0x78、垃圾输入报错** / **同 pkg+epoch 第二次提交被跳过、重启后仍去重、落盘文件能解回** /
key 路径安全 / 无目录时去重仍在内存里生效）。

[V] 真机 e2e（`tools/binder-probe/e2e-record.sh`，alioth，`UPERF_RECORD=1` +
`UPERF_RECORD_MIN_MS=4000` 让短跑也能产出会话）：

```
Rust: recorder enabled (min session 4000 ms, known 0)
Rust: recorder session com.android.settings     7100 ms (7 samples) -> …/history/com.android.settings-13916266.dfl
Rust: recorder session com.android.documentsui  8278 ms (8 samples) -> …/history/com.android.documentsui-13923366.dfl
history/: com.android.documentsui-13923366.dfl (145 B)  com.android.settings-13916266.dfl (136 B)
首 4 字节 8d cc cd 0a（不是 zlib 的 0x78）→ 裸 DEFLATE
```

[V] 把两个文件拉到主机、用 **python `zlib.decompressobj(-15)`** 解回：

```json
{"pkg": "com.android.settings", "epoch_ms": 13916266, "duration_ms": 7100,
 "samples": [{"t_ms": 0, "scene": "idle", "fps": 0.0}, {"t_ms": 1023, …}]}
{"pkg": "com.android.documentsui", "epoch_ms": 13923366, "duration_ms": 8278,
 "samples": [{"t_ms": 1023, …}]}
```

即：opt-in、时长规则、Deflate 格式、pkg+epoch 命名/去重，全部有证据。

## [U] / 未做

* 会话里 `fps` 全 0.0 —— 因为这次钉的 layer 名是编的（`Settings#0`，设备上不存在），
  帧腿量不到东西；帧腿正常工作时才会有非零值（⑤ 已单独验证）。
* 没有**容量/保留策略**（`history/` 会一直长）、没有清理命令。
* 没有对外读取通道（见上）；也没有"自测"的判定逻辑（本项只做**记录**，判读留着）。
