# M1 实施记录（**未在真机上跑通**）

> M1 范围：Rust 端起 staticlib + C ABI 桥跑通；C++ 启动 → 调 `uperf_rs_start` →
> 订阅 → 收到事件 → 在同一 sink 打日志。设备不在线，未做真机验证；本节
> 包含**所有本机可执行**的验收（cargo build / link / check / cargo test）。

## 1. Rust 侧（`rust/uperf-core/`）

| 文件 | 内容 |
|---|---|
| `Cargo.toml` | staticlib crate；依赖 `serde`, `serde_json`, `libc`, `once_cell`, `parking_lot`, `thiserror`（**无** crossbeam/channel — 改用 std mpsc） |
| `src/lib.rs` | `uperf_rs_init / start / reload / stop / on_event` 入口；`OnceLock<Bridge>` 保存桥句柄；`OnceLock<Mutex<Option<Dispatcher>>>` 保存分派线程 |
| `src/ffi.rs` | `Bridge` `#[repr(C)]` 表（`subscribe`, `write_log`），`#[no_mangle]` 不在里头；`DISPATCH_TX: OnceLock<Sender<Event>>` 给 FFI 入口发到分发线程 |
| `src/topic_dispatch.rs` | `Topic`（13 个，与 M0 tap 镜像同源）+ `Event::parse`（按 topic 与 payload 长度路由）+ `Dispatcher` 后台线程 |
| `tests/payload_decode.rs` | 10 个 payload 解码单元测试（**全部通过**，见 §4） |

### 关键 ABI 设计

```rust
// ffi.rs
#[repr(C)] pub(crate) struct Bridge {
    subscribe: unsafe extern "C" fn(*const c_char) -> c_int,
    write_log: unsafe extern "C" fn(*const c_char, usize),
}

// C++ 侧镜像表（cpp/include/uperf_rs_bridge.h）
typedef struct {
    int  (*subscribe)(const char *topic);
    void (*write_log)(const char *msg, size_t len);
} uperf_bridge_t;
```

C++ → Rust 的载荷布局（AGENT.md §5.1 实现）：

| topic | C++ 发出的载荷 | Rust 解析方式 |
|---|---|---|
| `input.touch / .btn / offscreen.state` | `int32_t` 0/1 | `decode_bool` |
| `input.state` | `int32_t in_hold, in_swipe, in_gesture`（12 字节） | `decode_input_state` |
| `topapp.pkgName` | 原始字节流（NUL 可有可无） | `std::str::from_utf8` |
| `cgroup.*.list` | `uperf_pid_list_t { const int32_t* pids; size_t len }`（16 字节结构指针） | `decode_pid_list`，复制到 `Vec<i32>` |
| `cgroup.*.update` | `data == NULL, len == 0` | 路由到 `Event::CgroupUpdate(Topic)` |

**`Rust` 永远不解引用 C++ 对象**（`std::string*`、`std::vector<int>*`、`InputData*`），所有 STL 解引用都在 C++ bridge 内完成。

## 2. C++ 侧（`cpp/uperf/bridge.cpp`）

- 在 CoBridge 上为 13 个 topic 各自安装一个 lambda（`install_subscribers`），payload 在 C++ 侧转成 C 布局后调 `uperf_rs_on_event`。
- 用 `uperf_bridge_t`（kBridge）作为过程级句柄；`uperf_rs_init` 把 `&kBridge` 交给 Rust。
- `uperf_bridge_write_log`：从 Rust 收来的字节流转 spdlog INFO 级，标签固定 `[Rust]`（M3 起按 logger 名）。
- **thread_local `pid_buf`**：把 `std::vector<int>` 复制成 `int32_t*`，指针与长度打包成 16 字节结构喂给 Rust；Rust 解包时拷贝——避免 Rust 解 STL 对象。
- `app_main.cpp::AppMainMayThrow` 调 `uperf_bridge_init_rust(config, log)` 把控制权交给 Rust 分派线程。

## 3. 构建产物

```
build/aarch64-linux-android23/runnable/uperf  895,416 B  (.85 MiB)   NEEL: libc.so libdl.so libm.so
rust/target/aarch64-linux-android/release/libuperf_core.a           8,539,664 B  (8.5 MiB)
```

`build.sh check` 全绿：动态 PIE、`NEEDED = libc/libdl/libm`、`GNU_RELRO + BIND_NOW`、stripped、< 3 MiB。Rust 静态库被链接进 ELF（链接成功，但 `--icf=all + --lto-O3 + --strip-all` 把所有函数名吃掉了——这是有意为之，与原版二进制形状一致）。

## 4. 单元测试（`cargo test -p uperf-core --release`，**真实运行**）

```
running 10 tests
test empty_pid_list_returns_empty_vec ... ok
test parse_cgroup_list ... ok
test parse_cgroup_update ... ok
test parse_input_btn_false ... ok
test parse_input_state ... ok
test parse_input_touch_true ... ok
test parse_offscreen ... ok
test parse_topapp ... ok
test short_pid_list_returns_empty ... ok
test unknown_topic_returns_none ... ok

test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

## 5. **真机验证 —— 待做**

设备（alioth `f748d277`）在 M1 工作收尾时断开（`adb devices` 返回空，USB bus 上只剩鼠标/键
盘/Bluetooth/声卡）。M0 那台真机一旦重新插上 USB，预期可执行：

```bash
adb push build/aarch64-linux-android23/runnable/uperf /data/local/tmp/uperf
echo '{"_m0_stub":"M1 lands"}' > /tmp/cfg.json
adb push /tmp/cfg.json /data/local/tmp/uperf_m0_stub.json
adb shell su -c '/data/local/tmp/uperf /data/local/tmp/uperf_m0_stub.json -o /data/local/tmp/m1_log.txt'
# 触发事件（滑动、切换 app）
adb shell su -c 'cat /data/local/tmp/m1_log.txt'   # 应出现 "Rust: ..." 行
adb shell su -c 'pkill -x uperf'
```

预期日志：每条 `Rust: <topic> = <value>` 行（详见 M0 §0 重做的格式）；M0 那台机
器上同时还存在原版 `EventTap:` 行（来自 fmt_logger、我在 M2 删除 fmt tap 之前会保留两者对照）。

## 6. 不在 M1 范围（按 AGENT.md §11）

- **配置解析**（63 份 JSON / 层叠覆盖 / 告警）—— M2
- **hint 状态机 + sysfs 写入器 + CPU governor + sched** —— M3-M4
- **sfanalysis listener / anim watcher** —— M5

M1 仅校验**"Rust ↔ C++ 的事件流跑通"**，没有施加任何策略；这是为了**逐层暴露问
题**（FFI / 载荷布局 / 线程模型 / 生命周期），不在策略还没写的时候让一切纠缠。