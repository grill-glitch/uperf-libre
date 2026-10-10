# uperf-libre

> 本文件是 uperf-libre 项目的**中文说明**。英文版见 [`README.md`](./README.md)。

## 简介

uperf-libre 是 [Uperf Game Turbo](https://github.com/yc9559/uperf)（闭源二进制，谱系：[Project WIPE](https://github.com/yc9559/cpufreq-interactive-opt) → [Project WIPE v2](https://github.com/yc9559/wipe-v2) → [Perfd-opt](https://github.com/yc9559/perfd-opt) → [QTI-mem-opt](https://github.com/yc9559/qti-mem-opt) → [Uperf v3](https://github.com/yc9559/uperf) → [Uperf Game Turbo](https://github.com/yinwanxi/Uperf-Game-Turbo)）的自由重写：用 Rust 替换闭源的 `magisk/bin/uperf` 二进制，C++ 平台层则直接复用上游同源的 [dfps](https://github.com/yc9559/dfps)（Apache-2.0）事件总线、worker 框架、inotify glue。

uperf-libre 保留原有的所有调用契约（`bin/uperf <USER_PATH>/uperf.json -o <USER_PATH>/uperf_log.txt`，`USER_PATH=/sdcard/Android/yc/uperf`），配置文件 schema v3 完整兼容，[`config/`](./config) 目录下 63 份平台配置可直接使用，不做任何修改。

---

## Why

Uperf v3（`dev-22.09.04`）是一个用户态 CPU 调频器 + 上下文调度器，并附带内核态升频做不到的一些特性（touch/swipe/SfAnalysis hint 状态机、动态 stune 风格 cluster 绑定、devfreq / LLCC boost）。原版以一个 stripped aarch64 PIE 二进制分发，意味着策略和能耗模型不可见，源代码永远无法在发布后重新审计，任何修改都得过原作者。

uperf-libre 是同样的对外表面，重写实现：

| 层              | 上游                                     | uperf-libre                                                                                  |
| --------------- | ---------------------------------------- | -------------------------------------------------------------------------------------------- |
| 二进制 `bin/uperf` | 闭源 C++，NDK r24，stripped              | Rust staticlib（`uperf-core`）+ dfps C++ 平台层（vendored, Apache-2.0）                       |
| 许可证          | 版权所有                                 | Apache-2.0                                                                                   |
| 源码            | 未公开                                   | 本仓库全量                                                                                   |
| Schema          | v3（`config/*.json`）                    | v3，**字节级兼容** —— 63 份平台配置全部接受                                                  |
| 日志            | dfps 风格 `H:M:S L message`              | 同一 line 格式                                                                               |
| 平台覆盖        | 63 份配置（`sdm855`、`kirin980` 等）    | 同一 63 份配置（`config/*.json`）；新增配置不影响既有数值                                    |
| SfAnalysis      | vendored 闭源 `libsfanalysis.so`         | `libsfanalysis_rs.so`（Rust cdylib，M8）—— mprotect + 内联 patch 钩 `xh_refresh_loop`，hint 文件协议字节兼容 |

不变的部分：

- 调用契约：`bin/uperf <config> -o <log>`。
- `USER_PATH=/sdcard/Android/yc/uperf` 以及 `magisk/script/libuperf.sh` 的所有调用契约。
- 进程名（`uperf`）、`killall uperf` 停止路径、以及 `cur_powermode.txt` / `perapp_powermode.txt` 切换文件。

变化的部分：

- Rust 内核可审计、可重新编译。
- 能耗模型、OPP → 频率策略、PL1/PL2 池算术、延迟平滑均有单元测试（`cargo test --release`）；闭源二进制的逐位取整/逐节拍选点无法复现，所以 uperf-libre 匹配 **配置语义** 与 **可观测行为**，不保证 1:1 重放。
- `AGENT.md §2` 基线默认配置使用偏保守的 PL1=2W。在 alioth 上会把小 cluster 钉到比 stock `schedutil` 更低的频点；可通过现有平台配置或自行编写调整。

---

## 功能与特性

这是一个用户态 CPU 调频器 + 上下文调度器，配合一个 hint 驱动的状态机。下文是每个 tick 的行为概述，参数定义详见 [`config/README.md`](./config/README.md)，真机证据见 [`docs/`](./docs)。

### CPU 与内存控制

- **CPU 频率通过 `userspace` 调速器 + `scaling_setspeed` 接管**。在 alioth（`qcom-cpufreq-hw`，`scaling_max_freq` 驱动级只读）上这是唯一可工作的路径；每个 policy 逐个被接管。详见 [`docs/m5-cpu-governor.md`](./docs/m5-cpu-governor.md)。
- **能耗模型驱动的 OPP 选择**。每个 cluster 的 power / cost 曲线在三个 cluster × 25 个 OPP 上与上游 printout 的拟合误差 <0.0015；然后在共享 PL1/PL2 池下做最优 OPP 选择。
- **Devfreq 与 LLCC boost** —— 对 DDR-bandwidth、L3-latency、CPU-LLCC-bandwidth、UFS devfreq 调速器的 `min_freq` / `max_freq` 写入。
- **cgroup、cpuset、devfreq 节点去重写入** —— 相同值重复写入会被跳过，从而把守护进程自身的 wakeup 开销压到最低。
- **外置看门狗 + 死手开关（M9）** —— `uperf_start` 会拉起一个 shell 守护：它用 `/proc/<pid>/exe` 识别 daemon（dfps 会把 supervisor 和 worker 的进程名都改写成 `uperf`，因此进程名与 pid 文件都不可信），并针对进程内无法自救的两种退出动手：`SIGKILL`，以及 `panic = "abort"` 下的 Rust panic。它会先 SIGTERM 收尾、恢复 daemon 记录过的 governor、在一个 per-boot 预算内重启 daemon；预算用尽后就让系统 governor 接管，而不是留下一个被钉死的频点。机器可读状态见 `<USER_PATH>/uperf.state`（daemon 写）与 `<USER_PATH>/uperf_watchdog.state`（看门狗写）；详见 [`docs/m9-watchdog.md`](./docs/m9-watchdog.md)。
- **Cluster 亲和性 / 动态 stune 风格绑定** —— 前台 App 的 UI 线程迁到大核，idle 与后台工作被排除在大核外。

### Hint 状态机

支持的场景简表，完整枚举见 [`config/README.md`](./config/README.md)。

- `None` —— idle 基线。
- `Tap` / `Swipe` / `Touch` / `Pressed` —— 从 `/dev/input/*` 事件派生，附带末端速度用于估算 swipe 持续时间。
- `HeavyLoad` —— 当系统负载度量 `Σ efficiency(i) × load_pct(i) × freq_mhz(i)` 超过 `heavyLoad` 至少 `requestBurstSlack` ms 时，从 touch hint 提升上来；负载降到 `idleLoad` 以下立即退出，过滤掉游戏（持续高负载）而保留短尖峰（App 启动、看图）。
- `SfLag` / `SfBoost` —— 来自注入到 `surfaceflinger` 的 SfAnalysis 模块；通过 token bucket 限频，避免长 GPU 停顿把 cluster 钉住。
- `AmSwitch` —— 前台 App 切换，通过 `ActivityManager` 活动检测；用于提前绑定新 App 的 UI 线程（负载迁移加快 ~100 ms）。
- `Standby` —— 屏幕熄灭 hint，通过 wake-lock 更新检测，不走框架广播。
- `WakeUp` —— 指纹 / 亮屏解锁序列；解锁动画期间升级到最大性能 hint。
- `SsAnim` —— 系统动画播放中（例：转场）。
- `RenderEnd` / `RenderRestart` —— 基于 surfaceflinger frame-submit 活动（每 sample 轮询）。让 hint 在最后一帧之后 200–300 ms 结束（用 SfAnalysis 可降到 66 ms），避免用户输入已结束而渲染未完成时的电费浪费。

### 配置与 UX

- **`config/*.json` schema v3**，与上游字节级兼容。
- **`magisk/script/libuperf.sh` 脚本契约**保留。
- **inotify 热重载** `cur_powermode.txt` 与 `perapp_powermode.txt`；分应用预设可通过 `Scene` 或 `sh /data/powercfg.sh <mode>` 配置。
- **WebUI**（KernelSU）位于 `webui/`，会被打入发布包。

### 故意未覆盖的部分

- **SfAnalysis** 已自研：上游闭源的 `libsfanalysis.so` 被替换为本仓库的
  `libsfanalysis_rs.so`（Rust cdylib，M8）。注入机制（mprotect + 内联 patch
  `libandroidfw.so` 内的 `xh_refresh_loop`）以 Rust 重写，hint 文件协议
  （`<USER_PATH>/sfanalysis.hint`，单字节 0..5）与上游消费端字节兼容。
  发布产物中不再携带任何闭源 .so。重写边界见
  [`docs/spec/sfanalysis.md`](./docs/spec/sfanalysis.md)，r2 静态逆向记录见
  [`docs/m8-sfanalysis-reverse.md`](./docs/m8-sfanalysis-reverse.md)。剩余的
  真机 byte 序列一致性对账是 M8 的最后一道验收。
- **APK 安装加速** 是上游通过一个独立事件支持的；未移植。

---

## 上游基线文档（保留原样）

以下章节保留 yinwanxi 撰写的 Uperf Game Turbo 原文档作为**实现细节参考**，便于对照原始意图理解每一项功能。uperf-libre 的对外身份声明以上文为准。

> 这是在 [Project WIPE](https://github.com/yc9559/cpufreq-interactive-opt)、[Project WIPE v2](https://github.com/yc9559/wipe-v2)、[Perfd-opt](https://github.com/yc9559/perfd-opt)、[QTI-mem-opt](https://github.com/yc9559/qti-mem-opt)之后的一个新项目。在之前的工作中，往往是基于一个现有的性能控制器做调参，这也意味着最后究竟能做到多好取决于控制器本身的上限。在EAS调度器成为主流之后无法应用WIPE系列的思路，因为EAS的参数自由度实在太少，等到借助了高通Boost框架才实现了更广范围的调整，才有了Perfd-opt。一方面受制于现有的性能控制器的功能限制，一方面还有一部分老设备没有这些新的性能控制器。没有条件就要创造条件，编写了一个安卓全平台的用户态性能控制器。
>
> 用户态性能控制通常有着较高的延迟（因为修改sysfs节点消耗相对较多的时间），但是距离实际应用场景很近可以在一些已知的重负载开始之前主动提升性能减少卡顿。一般的工作模式是在系统框架Java层发送Hint，由Native层的服务接收Hint并执行对应的sysfs修改，例如高通CAF Boost Framework、Power-libperfmgr。
>
> 与其他用户态性能控制器不同的是，Uperf没有Java层的部分，只有Native层接收时间通知和主动采样，这也就没有了系统框架层面的依赖。因此她不需要重新编译内核，也不需要修改Android框架源码，她也没有几乎硬件平台的限制。她的修改范围涵盖了所有内核态性能控制能够做到的，也就是说不用换掉没啥bug的官方内核，就能使用输入升频（没错，少部分老内核没有这个）、Dynamic Stune Boost、Devfreq Boost这些花式Boost。

下表为几个主要的性能优化方案的功能对比：

| 功能                  | Project WIPE | Perfd-opt(CAF) | libperfmgr | Uperf |
| --------------------- | :----------: | :------------: | :--------: | :---: |
| HMP+interactive       | ✔️          |            |        | ✔️   |
| EAS+schedutil         |              | ✔️          | ✔️      | ✔️   |
| 非高通平台            | ✔️          |            |        | ✔️   |
| Android < 8.0         | ✔️          |            |        | ✔️   |
| HMP模型自动调参       | ✔️          |            |        | ✔️   |
| UI线程的CPU亲和性     |              |            |        | ✔️   |
| 点击升频              |              |            | ✔️      | ✔️   |
| 列表滚动升频          |              | ✔️          | ✔️      | ✔️   |
| APP启动加速           |              | ✔️          | ✔️      | ✔️   |
| APK安装加速           |              | ✔️          |        |       |
| 待机优化              |              |            |        | ✔️   |
| 帧渲染滞后            |              |            |        | ✔️   |
| 渲染开始、结束        |              |            |        | ✔️   |
| surfaceflinger复杂合成 |              |            | ✔️      |       |
| 视频录制情景          |              |            | ✔️      |       |
| 多性能模式            | ✔️          | ✔️          |        | ✔️   |

### 情景识别

注：v3版本已经修改，此部分不适用
Uperf支持如下几种情景识别：
- `None`，无Hint的常规状态
- `Touch`，触摸到屏幕切换的Hint
- `Pressed`，长按时切换的的Hint
- `Tap`，在刚触摸到屏幕切换的Hint
- `Swipe`，在屏幕滑动一段距离后切换的Hint
- `HeavyLoad`，在Tap或Swipe检测到重负载后切换，负载降低后回落到Tap
- `SfLag`，给Surfaceflinger的渲染提交出现滞后切换的Hint
- `SfBoost`，Surfaceflinger的渲染提交需要加速切换的Hint
- `Standby`，屏幕熄灭时的Hint，一般滞后20秒(隐藏Hint)
- `SsAnim`，系统动画播放切换的Hint
- `WakeUp`，亮屏解锁切换的Hint

#### 触摸信号识别

本程序采用了跟安卓系统框架获取触摸信号一样的方式，监听位于/dev/input的设备，解析来自触摸屏的报点信息，可以获取到最基本的手指触摸到屏幕和手指离开屏幕的事件。根据一段连续的报点信息可以得到手指滑动的距离以及离开屏幕时末端速度，由此可以推断是点击操作还是滑动操作，以及根据末端速度推算APP滚动的持续时间。

#### 重负载跟踪与限制

因为不在安卓框架层插入Hook无法确切知道APP正在启动，因此本程序在Hint开始后，用主动轮询的方式更新所有CPU核心的使用率和运行频率得到系统整体负载。`系统整体负载 = sum(efficiency(i) * (load_pct(i) / 100) * (freq_mhz(i) / 1000))`，其中`i`为CPU核心ID。如果整体负载高于`heavyLoad`，那么把当前Hint切换到重负载Hint。重负载Hint的响应性能很好但耗电也偏多，本程序会持续监测系统负载，如果整体负载低于阈值，提前结束耗电的重负载Hint。对于负载不是那么高的APP热启动，甚至不会触发重负载，不像高通Boost框架不管负载多少强行拉满CPU持续2s。此外，这样的检测不仅涵盖了APP冷热启动，还涵盖了例如点击进入微信朋友圈这样的短时重负载场景。本功能的能耗开销也是在非常低的0.6ms/100ms（Cortex-A55@1.0g）。下图为微信热启动Hint状态切换与持续时间。

![微信热启动](media/wechat_resume.png)

某些的游戏负载确实非常高，系统负载能够非常稳定的持续超过阈值。理论上重负载游戏应该运行在功耗拐点的频率上，保持足够的性能输出的同时才不会发热过大，这与突发重负载的设置初衷矛盾。因此限制了请求进入重负载Hint的请求间隔，在上一次HeavyLoad结束后，负载低于`idleLoad`保持1秒，并且在`requestBurstSlack`这段时间内没有HeavyLoad请求，才能响应新的HeavyLoad，也就过滤了游戏这类持续重负载能耗过高的问题。

#### 正在操作的APP发生切换

基于能够响应上面这些主要的事件，完全可以把非操作时的参数设置的比以前更加保守而不用担心卡顿，但是点亮唤醒是个例外。屏幕下指纹在息屏显示时，按压指纹传感器完成解锁这个操作就算触摸到了屏幕也没有input事件。而点亮屏幕的动画过程往往伴随着大量进程唤醒，保守的参数会造成显著卡顿。本程序通过监测安卓框架的ActivityManager的活动，ActivityManager在正在交互的APP发生变化、解锁屏幕、锁定屏幕时会非常活跃，由此可以推断是否发生了解锁屏幕事件。通过这一监测，还可以实现在APP切换或者启动时，把APP更早的放到大核心，负载迁移延迟可以降低大约100ms。本功能由事件驱动，几乎没有额外的能耗开销。下图为光学屏幕指纹解锁过程。

![光学屏幕指纹解锁过程](media/android_am.png)

#### 识别屏幕熄灭

在以往的Project WIPE和Perfd-opt中，很多用户借助Scene工具箱实现熄屏后自动切换到省点模式降低一点待机耗电。处于Native层的Uperf无法像Scene工具箱一样收到系统的熄屏广播，而是监听唤醒锁更新操作来识别屏幕是否熄灭。

#### SfAnalysis

Sfanalysis是一个独立于Uperf的模块，注入到surfaceflinger进行修改，从这个负责Android所有帧渲染提交的进程发出信号，通知Uperf调整性能输出，在观察到卡顿之前就提升性能，真正做到未卜先知，这是所有内核态升频所不能企及的。然而想要她的实现有诸多限制，OEM可以改源码，做内核的可以改内核源码，Uperf为了普适性不能修改源码。如果使用注入方式，surfaceflinger是native进程，使用C++编写，相比system_server这类Java写成的hook位点更少，更不用提不同Android版本的实现还不一样。就算注入成功，由于Android对系统进程设置了很多SELinux规则，防止被注入攻击后取得太多的权限，通知信号也难以发出。绕过了这些限制后，Sfanalysis具有以下功能：

- hook关键调用，推测并向外部传递渲染开始、渲染提交滞后、渲染结束事件
- 自适应动态刷新率、自适应vsync信号滞后间隔
- 在SELinux的权限范围内，向外部传递信号，因此不需要关闭SELinux才能使用

![检测到渲染延迟立即拉升CPU频率](./media/sflag.png)

渲染提交滞后对应的Hint`SfLag`与重负载一样，有调用频率限制避免长时间拉升高频，相关参数暂时没有开放更改。`SfLag`使用可用次数缓冲池控制调用频率，每满400ms间隔可用次数+1，最大到20次。为了避免不必要的频率拉升，只允许从`Tap`、`Swipe`、`Touch`、`Pressed`转移到`SfLag`。SfAnalysis正常工作后在日志以如下方式体现：

```
[13:03:36][I] SfAnalysis: Surfaceflinger analysis connected
```

#### 渲染结束提前结束Hint

即使有了触摸末端速度推算，由于每个设备的滑动阻尼不同，实际的渲染持续时间也大不相同，套用固定值容易导致电量浪费。在内核态boost可以通过在drm/atomic添加hook实现渲染结束后提前结束Boost，本程序也使用类似的方法，在渲染结束后200-300ms内结束Hint的响应，覆盖全程UI渲染过程的同时减少电量浪费。本程序在Hint开始后，使用主动轮询的方式监控安卓的surfaceflinger活动，几乎所有版本的安卓的渲染提交都经过它，同时能耗开销在非常低的0.4ms/100ms（Cortex-A55@1.0g）。

使用SfAnalysis渲染结束信号之后，提前结束Hint的延迟可以进一步降低到66ms。
![渲染停止](media/render_stop.png)

在尽可能缩短渲染结束提前结束的滞后的同时，会导致某些UI响应本身存在滞后的场景发生太多卡顿，因为Hint已经提前退出。此类情况大多发生在浏览信息流点击图片切换到全屏显示图片的过程。因此在提前结束Hint的同时，还需要检测是否有滞后的UI响应，在点击的700ms以内如果重新开始渲染会恢复先前的Hint。使用主动轮询的方式监控安卓的surfaceflinger活动，恢复Hint的延时在100ms以上，使用SfAnalysis渲染开始信号之后延迟可以进一步降低到33ms。

![滞后UI渲染开始](media/render_restart.png)

### 写入器

写入器基本功能是把目标字符串值写入到`sysfs`节点，除此以外，Uperf还内建了多种写入器实现了其他功能和更加紧凑的参数序列。在切换动作时，Uperf会比对与上一动作参数值的差异，跳过写入重复的值来减少自身功耗开销。Uperf支持的`knob`有如下几种类型：
- `string`，最基础的写入器。效果等同于`echo "val" > /path`。
- `percluster`，分集群紧凑型写入器。使用配置文件中`platform/clusterCpuId`的CPU序号替换`path`中的`%d`，各个值由逗号分隔，使得按集群做区分的值更加紧凑，改善可读性。
- `percpu`，分核心紧凑型写入器。根据配置文件中`platform/efficiency`的列表长度，生成CPU序号替换`path`中的`%d`，各个值由逗号分隔，使得按CPU核心做区分的值更加紧凑，改善可读性。
- `cpufreq`，`percluster`写入器的变种。大部分功能相同，不同的是写入值=设定值*100000，缩短了频率参数序列的长度，以及带有写入失败重试以处理新的最低频率高于原有的最高频率。
- `cgroup_procs`，Cgroup专用写入器。支持最大4个值，各个值由逗号分隔，设定值为进程名称，Uperf在初始化时会扫描系统所有进程，用匹配到的第一个PID替换它们。一般用于设置系统关键进程到指定的cgroup。由于一个进程的线程可能会动态变化，因此此类写入器会关闭去重。
- `uxaffinity`，UxAffinity写入器。在每次正在操作的APP发生切换时，Uperf都会扫描属于顶层APP的cgroup的所有线程，缓存所有UI相关线程的ID。当设定它为1时，把UI相关线程固定到大核心。当设定它为0时，允许UI相关线程使用全部可用核心。在大多数EAS平台上设置`schedtune.boost > 0`和`schedtune.prefer_idle = 1`即可把任务固定到大核，但是EAS在各个平台的具体实现层次不齐，这个参数组不合适用于所有EAS平台。为了解决这一问题，Uperf主动设置这些关键线程的CPU核心亲和性，适用于所有EAS平台，甚至是HMP平台。

### 预调参

- Uperf模块为大多数热门硬件平台提供了调参后的配置文件，以尽可能发挥Uperf的优势
- HMP平台均衡和卡顿版的`interactive`参数与HMP负载迁移阈值由改进的[Project WIPE v2](https://github.com/yc9559/wipe-v2)提供，费电模式采用固定在功耗拐点的频点提供最稳定持续的性能
- EAS平台的频点选择综合了SOC功耗模型和常见负载的性能需求，由一套固定策略生成
  - 三星、sdm845以及移植的EAS平台，由于缺少关键内核功能采用传统的调参方法，即普通场景不提供过高的性能容量
  - sdm845以后的高通EAS平台，采用调整后性能需求-性能容量模型，见下图

![调整后性能需求-性能容量模型](media/adjusted_demand_capacity_relation.png)

假设系统负载只由单个任务贡献。EAS默认的方式由于`schedutil`总是预留25%性能余量，而SOC的不同频点的能耗比表现不同，越接近最大频率能耗比越低，EAS的默认策略会导致较高负载时最大频率占比偏大。从现实负载变化的规律来看，25%性能余量并不总是够用，负载较低时容易产生大的波动，负载较高时性能需求反而是相对稳定的。从SOC的功耗模型和现实负载变化的规律来看，负载较低时由于波动值的相对百分比较大应该留出更大的性能余量，SOC的低频段一般能耗比差别不大，功耗负面影响不大；负载较高时由于波动值的相对百分比较小应该留出较小的性能余量，SOC的高频段的每个频点之间的能耗比差别比较明显，功耗正面影响较大。

### 外围改进

本模块除了Uperf本体以及SfAnalysis注入，还配合一些外围的改进共同改进用户体验。
- Uperf启动前其他参数统一化，包括：
  - schedtune置零
  - 使用CFQ调速器，降低多任务运行时前台任务的IO延迟
  - 降低非前台APP的IO带宽占用权重
  - 设置与UI性能密切相关的系统进程到顶层APP的cgroup分组
  - 固定于过渡动画相关的线程到大核
  - 减少大部分传感器线程在大核的唤醒
  - 禁用大多数内核态和用户态boost、热插拔
  - `interactive`和`schedutil`调速器、`core_ctl`、任务调度器外围参数一致化
- 为指纹识别提供最大性能(EAS平台)

![为指纹识别提供最大性能](./media/fingerprint.png)

## 自定义配置文件

本项目已经为大多数热门硬件平台提供了调参后的Uperf配置文件，但总有一些情况预调参的配置不适用于你的软硬件平台，例如冷门的硬件平台、自定义的内核。此外，也有自定义现有预调参配置文件的需求，例如调高交互时的最低CPU频率、增加GPU频率范围调整。在Uperf设计之初便考虑到了这类需求，开放几乎所有的可调参数，并且在配置文件更改保存后自动重新加载，改善在手机端调试参数的效率。Magisk模块使用的配置文件位于`/sdcard/yc/uperf/cfg_uperf.json`。

### 元信息

```json
"meta": {
    "name": "sdm855/sdm855+ v20200516",
    "author": "yc@coolapk",
    "features": "touch cpuload render standby sfanalysis"
}
```

| 字段名   | 数据类型 | 描述                                           |
| -------- | -------- | ---------------------------------------------- |
| name     | string   | 配置文件的名称                                 |
| author   | string   | 配置文件的作者信息                             |
| features | string   | 配置文件支持的功能列表，目前是保留字段不起作用 |

`name`与`author`在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Using [sdm855/sdm855+ v20200516] by [yc@coolapk]
```

### 全局参数

```json
"common": {
    "switchInode": "/sdcard/yc/uperf/cur_powermode",
    "verboseLog": false,
    "uxAffinity": true,
    "stateTransThd": {
        "heavyLoad": 1500,
        "idleLoad": 1000,
        "requestBurstSlack": 3000
    },
    "dispatch": [
        {
            "hint": "None",
            "action": "normal",
            "maxDuration": 0
        },
        {
            "hint": "Tap",
            "action": "interaction",
            "maxDuration": 1500
        },
        ...
    ]
}
```

| 字段名            | 数据类型 | 描述                                                                            |
| ----------------- | -------- | ------------------------------------------------------------------------------- |
| switchInode       | string   | 接收性能模式切换的inode节点                                                     |
| verboseLog        | bool     | 开启详细日志，用于调试Hint切换                                                  |
| uxAffinity        | bool     | 开启UX线程自动设置，固定高优先级的UX线程到大核，并限制低优先级线程的需求响应    |
| heavyLoad         | int      | 进入重负载的系统负载阈值，详见[重负载跟踪与限制](#重负载跟踪与限制)             |
| idleLoad          | int      | 退出重负载的系统负载阈值，详见[重负载跟踪与限制](#重负载跟踪与限制)             |
| requestBurstSlack | int      | 单位毫秒，响应新的重负载请求前的延时，详见[重负载跟踪与限制](#重负载跟踪与限制) |
| hint              | string   | 对应到Uperf内部支持的Hint类型                                                   |
| action            | string   | 绑定的动作名称，可以自定义                                                      |
| maxDuration       | int      | 单位毫秒，动作保持的最大时长                                                    |

在Uperf启动时会读取`switchInode`对应路径的文件获取默认性能模式,在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Read default powermode from /sdcard/yc/uperf/cur_powermode
[13:03:33][I] CfgMgr: Powermode "(null)" -> "balance"
```

`switchInode`对应路径的文件，监听新模式名称的写入完成模式切换：

```shell
echo "powersave" > /sdcard/yc/uperf/cur_powermode
```

在日志以如下方式体现：

```
[13:06:45][I] CfgMgr: Powermode "balance" -> "powersave"
```

`dispatch`的绑定关系，在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Bind HintNone -> normal
[13:03:33][I] CfgMgr: Bind HintTap -> interaction
[13:03:33][I] CfgMgr: Bind HintSwipe -> interaction
[13:03:33][I] CfgMgr: Bind HintHeavyLoad -> heavyLoad
[13:03:33][I] CfgMgr: Bind HintAndroidAM -> amSwitch
[13:03:33][I] CfgMgr: Bind HintStandby -> standby
[13:03:33][I] CfgMgr: Bind HintSflag -> sfLag
```

`UxAffinity`和`SfAnalysis`这两项功能在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: UX affinity enabled
...
[13:03:36][I] SfAnalysis: Surfaceflinger analysis connected
```

### 平台信息

```json
"platform": {
    "clusterCpuId": [
        0,
        4,
        7
    ],
    "efficiency": [
        120,
        120,
        120,
        120,
        220,
        220,
        220,
        240
    ],
    "knobs": [
        {
            "name": "cpuFreqMax",
            "path": "/sys/devices/system/cpu/cpu%d/cpufreq/scaling_max_freq",
            "type": "cpufreq",
            "enable": true
        },
        ...
    ]
}
```

| 字段名       | 数据类型    | 描述                                                                  |
| ------------ | ----------- | --------------------------------------------------------------------- |
| clusterCpuId | int list    | 多集群CPU每个集群的首个CPU ID                                         |
| efficiency   | int list    | 每个CPU核心的的相对同频性能，以Cortex-A53@1.0g为100，顺序与CPU ID对应 |
| knobs        | object list | `sysfs`节点列表                                                       |

`knobs`中的每个对象为`knob`，有以下属性：

| 字段名 | 数据类型 | 描述                                   |
| ------ | -------- | -------------------------------------- |
| name   | string   | `sysfs`节点名称                        |
| path   | string   | `sysfs`节点路径                        |
| type   | string   | `sysfs`节点类型，详见[写入器](#写入器) |
| enable | bool     | 是否启用，方便调试时一键禁用           |

当`enable`字段为false时，在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Ignored root/platform/knobs/topCSProcs [Disabled by config file]
```

当`path`字段对应的`sysfs`节点不存在或者不可写入时，在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Ignored root/platform/knobs/bigHifreq [Path is not writable]
```

### 性能模式参数

```json
"powermodes": [
    {
        "name": "balance",
        "actions": {
            "interaction": {
                "cpuFreqMax": "18,18,22",
                "cpuFreqMin": "10,10,8",
                "cpuLoadBoost": "0,0,0,0,0,0,0,0",
                "fgCpus": "0-3",
                "topCSProcs": "com.android.systemui,system_server",
                "fgSTProcs": "system_server",
                "ddrBwMax": "6000",
                "ddrBwMin": "2500",
                "uxAffinity": "1"
            },
            ...
        },
        ...
    },
    ...
]
```

| 字段名     | 数据类型 | 描述                                      |
| ---------- | -------- | ----------------------------------------- |
| name       | string   | 可自定义，用于备份调参的多个版本          |
| 动作名称   | string   | 与`common/dispatch`中定义的动作名对应     |
| `knob`名称 | string   | 与`platform/knobs`中定义的`sysfs`节点名称 |
| `knob`值   | string   | 值的格式详见[写入器](#写入器)             |

一个动作应该为所有在`platform/knobs`定义的`knob`设置值。某些时候需要故意跳过某些值的设定，或者复用大部分前一动作的设定值，可以省略部分`knob`设置值，但不能全部。Uperf在加载配置文件时会提示哪些值没有设定会被跳过，在日志以如下方式体现：

```
[13:03:33][I] CfgMgr: Ignored knobs in action root/powermodes/balance/actions/amSwitch:
[13:03:33][I] CfgMgr: cpuFreqMin llccBwMax llccBwMin ddrBwMax ddrBwMin l3LatBig ddrLatBig
```

### 示例

利用Uperf为交互和重负载添加关闭UFS节能，以此降低性能关键场景的IO瓶颈问题。

UFS节能开关的`sysfs`节点路径为`/sys/devices/platform/soc/1d84000.ufshc/clkgate_enable`，接收字符串类型写入，写入"0"为关闭UFS节能，写入"1"为开启UFS节能，把这一节点取名为`ufsClkGateEnable`。在配置文件添加如下文本完成`knob`定义：

```json
"platform": {
    ...
    "knobs": [
        ...
        {
            "name": "ufsClkGateEnable",
            "path": "/sys/devices/platform/soc/1d84000.ufshc/clkgate_enable",
            "type": "string",
            "enable": true,
            "note": "UFS时钟门开关"
        },
        ...
    ]
}
```

根据[情景识别](#情景识别)中的定义，交互的hint名称为`Tap`和`Swipe`，重负载的hint名称为`HeavyLoad`。

```json
"dispatch": [
    ...
    {
        "hint": "Tap",
        "action": "interaction",
        "maxDuration": 1500
    },
    {
        "hint": "Swipe",
        "action": "interaction",
        "maxDuration": 3000
    },
    {
        "hint": "HeavyLoad",
        "action": "heavyLoad",
        "maxDuration": 2000
    },
    ...
]
```

根据配置文件内定义的hint与动作的绑定关系，需要给动作`interaction`和`heavyLoad`设置关闭UFS节能，其他动作保持开启UFS节能。在配置文件添加如下文本完成动作定义：

```json
"powermodes": [
    {
        "name": "balance",
        "actions": {
            "normal": {
                ...
                "ufsClkGateEnable": "1",
                ...
            },
            "interaction": {
                ...
                "ufsClkGateEnable": "0",
                ...
            },
            "heavyLoad": {
                ...
                "ufsClkGateEnable": "0",
                ...
            },
            "amSwitch": {
                ...
            },
            "standby": {
                ...
                "ufsClkGateEnable": "1",
                ...
            },
            "sfLag": {
                ...
            },
        },
    },
    {
        "name": "powersave",
        "actions": {
            "normal": {
                ...
                "ufsClkGateEnable": "1",
                ...
            },
            ...
        },
    },
    ...
]
```

更改配置文件后保存，Uperf会自动创建新的子进程加载新的配置文件，如果新的配置文件格式存在问题，会终止新的子进程保留老的子进程。接下来验证配置文件中设定动作是否能如期执行，对应路径的值是否发生更改。

## 构建与安装

```sh
export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make check
```

产物落到 `build/aarch64-linux-android23/runnable/uperf`。`make check` 目标**先**跑 `cargo test --release`（Rust 内核）再调 CMake，因为 FFI 签名变更后若 `libuperf_core.a` 是旧版本会在 `memcpy` 里 segfault（见 `AGENT.md §10` 与 [`docs/`](./docs) 中的真机证据）。

Magisk 模块打包命令沿用上游约定；`build.sh pack` 产出一个 KernelSU 兼容 zip，落到下次开机时的 `/data/adb/modules_update/uperf-libre`。模块 id、路径、以及 `libuperf.sh` 脚本契约全部保留。

不使用 Magisk/KernelSU 时：把 zip 解到设备任意目录，执行 `sh <解压目录>/script/setup.sh`，它会把匹配到的 SoC 配置播种到 `$USER_PATH/uperf.json`，并在改动任何东西之前打印作者、许可证与免责声明。然后用模块相同的方式启动守护进程：

```sh
<解压目录>/bin/uperf /sdcard/Android/yc/uperf/uperf.json -o /sdcard/Android/yc/uperf/uperf_log.txt
```

（`script/initsvc.sh` 是开机入口，做同样的事并额外完成平台修正。M9 看门狗属于模块的
`uperf_start`/`uperf_stop` 路径，手工启动的 daemon 没有监督者 —— 想要死手开关就用模块。）

### 验证

装完后，确认守护进程已起：

```sh
cat /sdcard/Android/yc/uperf/uperf_log.txt | tail
echo powersave > /sdcard/Android/yc/uperf/cur_powermode   # 热重载
```

优雅停止 —— 模块自己的路径：先停看门狗（否则它会把这次停止当成崩溃、把 daemon 再拉起来），再停 daemon，最后恢复：

```sh
killall uperf
```

硬杀不再让设备卡死。看门狗会发现在某个 policy 仍读作 `userspace` 时 daemon 已经不在，于是恢复记录过的原 governor，并在一个 per-boot 预算内重启 daemon；预算用尽就让系统 governor 接管并写明原因。用状态文件观察：

```sh
cat <USER_PATH>/uperf.state            # daemon 写的 state / takeover / armed / policies
cat <USER_PATH>/uperf_watchdog.state   # 重启次数、最后一次动作及原因
tail <USER_PATH>/uperf_watchdog.log    # 看门狗的每一次判定
```

同一份状态在 KernelSU WebUI 首页新增的**守护**卡片里直接可见（看门狗状态/PID/重启次数、守护进程自报、已接管集群、以及"已被杀"判定），也通过 adb 的 `sh <module>/script/webui.sh status` 暴露：
`daemon.state`、`daemon.armed`、`watchdog.state`、`watchdog.restarts`、`watchdog.pid`。

`uperf.state` 里残留 `state=running` 但没有存活进程，就是被杀的标志；正常停止会写 `state=stopped`。`UPERF_WATCHDOG=0` 可关闭看门狗（排查崩溃循环时有用）。

如果设备在手边没有看门狗的情况下卡在 `userspace`（手工删了模块、或关掉了看门狗），恢复：

```sh
for d in /sys/devices/system/cpu/cpufreq/policy*; do
  echo powersave > $d/scaling_governor
done
```

启动脚本在启动时把替换的 governor 记到 `<USER_PATH>/orig_governor.txt`，且**从不臆造**值：没有记录的 policy 会被原样留下并如实报告。因此正常 `killall uperf`、以及看门狗的死手路径，都会自动恢复 `schedutil`。

## 平台覆盖

63 份平台配置位于 [`config/`](./config)。每份都是上游二进制的直接替换：`setup.sh build.sh sdm888.json` 与原 `yinwanxi/Uperf-Game-Turbo` Magisk 模块的调用完全一致。

不在列表里的设备，二进制仍能运行，但会跳过 SoC 专属 knob（`modules.sysfs.knob` 解析为 `None`）；新增 SoC 配置方法见 `AGENT.md §11`。

## 架构

详见 [`AGENT.md`](./AGENT.md)（整体方案 + 验收标准）与 [`docs/`](./docs)（每个里程碑的真机证据）。

```
┌─────────────────────────────────────────────────────────────┐
│ C++（vendored dfps, Apache-2.0, 进程骨架）                    │
│  main.cpp        监督器：fork worker, SIGCHLD tombstone      │
│                  SIGUSR1 优雅重启, spdlog                    │
│  platform/       ModuleBase, CoBridge, DelayedWorker,        │
│                  HeavyWorker, Inotifier, Singleton           │
│  modules/        InputListener CgroupListener OffscreenMonitor│
│                  TopappMonitor      ← 事件源                 │
│  utils/          inotify input_reader sched_ctrl atrace …   │
├─────────────────────────────────────────────────────────────┤
│ extern "C" 桥（cpp/include/uperf_rs.h）                      │
├─────────────────────────────────────────────────────────────┤
│ Rust（本项目重写, staticlib libuperf_rs.a）                   │
│  app      模块装配（替代 uperf.cpp）                         │
│  config   JSON 解析 + 点号键覆盖 + 兼容告警                  │
│  switcher hint FSM + preset/perapp 切换 + 时长               │
│  profile  preset/scene → 各模块参数表下发                    │
│  sysfs    6 类写入器 + 去重 + fd 缓存                        │
│  governor 负载采样 + 能耗模型 + PL1/PL2 池 + 频点决策       │
│  sched    上下文调度规则引擎（PCRE2 语义的 ERE）             │
│  sf       sfanalysis.hint 消费 + 渲染结束/Hint 提前结束     │
└─────────────────────────────────────────────────────────────┘
```

CPU 调频器通过将每个 `cpufreq` policy 的 `scaling_governor` 切到 `userspace`、然后每 ~20 ms 通过 `scaling_setspeed` 发布下一个 OPP 目标来接管。在驱动层锁住 `scaling_max_freq` 的场景（例：`qcom-cpufreq-hw` on alioth —— mode 444，root 写入返回 EACCES），这是唯一可工作的路径；详见 [`docs/m5-cpu-governor.md`](./docs/m5-cpu-governor.md) 与 [`rust/uperf-config/src/freq_target.rs`](./rust/uperf-config/src/freq_target.rs) 中的解析顺序。

## 状态

- **稳定**：上游基础参考（25 个 OPP 上与 printout 拟合误差 <0.0015）、能耗模型、PL1/PL2 池算术、scene → sysfs 写入流水线，以及基于 inotify 的 `cur_powermode.txt` / `perapp_powermode.txt` 热重载。
- **守护（M9）**：主机侧已验证（`cargo test --release` 211 个测试；`sh scripts/test_watchdog_host.sh` 10 用例 / 60 条断言），alioth **真机验证**（`scripts/m9-device-verify.sh`，33 条断言：`/proc/<pid>/exe` 穿过 dfps 的 cmdline 改写、SIGKILL 后死手路径恢复真实已接管 daemon、孤儿 worker 被 SIGTERM 后自行 disarm、出厂 15 s 节奏下占单核 0.93–1.12%），并完成 **装机 + 重启验收**（`scripts/m9-device-boot-verify.sh`，`ksud module install` + 重启后 23 条断言：KernelSU 的 `service.sh` 路径确实拉起看门狗且活过重启；`webui.sh restart` 把 owner 锁干净交接给新实例）。过程中发现并修掉五个**只有真机才暴露**的缺陷（扫描开销、进程名过滤器、无换行文件读取、`gave-up` 被后续采样擦除、以及重启后状态文件可能残留 `stopped` 而 daemon 仍在跑）。见 [`docs/m9-watchdog.md`](./docs/m9-watchdog.md) §5。
- **日志上限（M10）**：daemon 自己轮转日志（每文件 4 MiB、保留 2 个），启动路径对超限备份直接丢弃（16 MiB）而不是去读它——改前实测：34 MB 的日志加 138 MB 的 `.bak` 堆在 `/sdcard`。见 [`docs/m10-log-cap.md`](./docs/m10-log-cap.md)。
- **尽力而为**：延迟平滑（每采样最多一步，除非 predict 触发 —— 上游描述的是连续共享延迟预算，离散近似无法做到逐节拍匹配），以及 guideCap / limitEfficiency 的容量裁剪表（不可直接从闭源二进制观测）。
- **真机验证**：alioth（crDroid Android 16 / KernelSU Next 3.3.0）与 polaris（LineageOS 22.2 working；Android 16 / 4.19 内核 —— axion 配置 + `KERNEL_CLANG_TRIPLE`）。

## 贡献

提交 PR 前请阅读 [`AGENT.md §10 验收清单`](./AGENT.md)。项目内置 parity 工具（`rust/uperf-cli`）会逐字节比对 `Config '{}' by '{}'` 与 `Knob '{}' not writeable` 日志行与闭源二进制期望输出，覆盖所有内置配置，警告集合一旦漂移 CI 即失败。

## 致谢

- 闭源的 Uperf v3 二进制、配置与平台脚本：[yinwanxi/Uperf-Game-Turbo](https://github.com/yinwanxi/Uperf-Game-Turbo)（`b13d54a`）。
- SfAnalysis 注入源码来自 [yc9559/surfaceflinger-analysis](https://github.com/yc9559/surfaceflinger-analysis)；r2 静态逆向（`docs/m8-sfanalysis-reverse.md`）基于该项目 `dev-22.09.04` release 二进制完成。
- dfps C++ 平台层，vendored 自 [cpp/dfps/](./cpp/dfps)（取自 [yc9559/dfps](https://github.com/yc9559/dfps)，Apache-2.0）；vendoring 规则见 `cpp/dfps/DFPS_VENDOR.md`。
- 63 份平台配置由各自作者贡献，署名见每份配置的 `meta.author` 字段。

感谢以下用户或项目的源码对本项目的帮助：
- [@AndroidDumps](https://github.com/AndroidDumps)
- [TinyInjector](https://github.com/shunix/TinyInjector)
- [xHook](https://github.com/iqiyi/xHook)
- [@cjybyjk](https://github.com/cjybyjk)
- [@SatySatsZB](https://github.com/SatySatsZB)
- [@osm0sis](https://github.com/osm0sis)
- @YMJ

感谢以下用户的测试反馈和错误定位：
- @HEX_Stan(coolapk)
- @僞裝灬(coolapk)
- @Yoooooo(coolapk)
- @我愿你安i(coolapk)
- @鹰雏(coolapk)
- @yishisanren(coolapk)
- @asd821385525(coolapk)
- @倚楼醉听曲(coolapk)
- @NepPoseidon(coolapk)
- @寻光丿STLD(coolapk)
- @比企谷の雪乃(coolapk)
- @非洲咸鱼(coolapk)
- @哔哩哔哩弹慕网(coolapk)
- @我心飞翔的安(coolapk)
- @浏泽仔(coolapk)
- @〇MH1031(coolapk)
- @今天我头条了吗(coolapk)
- @瓜瓜皮(coolapk)
- @Universes(coolapk)
- @Superpinkcat(coolapk)
- @asto18089(coolapk)
- @顺其自然的肥肉(coolapk)
- @酷斗吧(coolapk)
- @何为永恒(coolapk)
- @我为啥叫这个(coolapk)
- @goddard(coolapk)
- @正果sss(coolapk)
- @Cowen(coolapk)
- @瞬光飞翔(coolapk)
- @kuiot(coolapk)
- @常凯申将军(coolapk)
- emptybot08(github)
- ahzhi(github)
- Saumer7(github)

## 免责声明

**无任何担保，使用风险自行承担。** 本模块以 root 身份运行，并写入内核与系统可调项（cpufreq、cpuset、调度器放置、温控与 devfreq 节点）。配置不当、内核拒绝某次写入，或异常关机，都可能让设备停在只有重启、recovery 或重刷才能恢复的状态。安装前请确认你能在没有本模块的情况下开机（Magisk/KernelSU 安全模式）。

本项目以 Apache-2.0 **按原样**分发，不提供任何明示或默示担保，包括对适销性或特定用途适用性的默示担保（Apache-2.0 §7-8）。安装脚本在改动任何东西之前会打印同样的声明。本项目与上游作者 Matt Yang（yc9559）、yinwanxi 没有隶属或背书关系。

模块**不附带任何闭源第三方二进制**：zip 里仅有的非文本文件是本仓库自行构建的产物，以及在 [`NOTICE`](./NOTICE) 中声明的 GPL-2.0 Android busybox。

## 许可证

Apache-2.0。详见 [`LICENSE`](./LICENSE) 与 [`NOTICE`](./NOTICE)。
