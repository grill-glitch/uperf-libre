# M5 — userspace CPU governor (energy model + control path)

Status legend: **[V]** verified by tool output · **[I]** inferred · **[U]** unknown.

## 1. The energy model — solved exactly [V]

The upstream binary prints its own model table at startup:

```
CpuGovernor cluster0:
opp  614400 pwr 0.053 cost 0.075
...
opp 1804800 pwr 0.302 cost 0.146
```

Fitting those numbers (25 OPPs across 3 clusters of `config/sdm888.json`) gives a
closed form. With

```
x  = freq_GHz / typicalFreq        Rp = plainFreq / typicalFreq        Rs = sweetFreq / typicalFreq

            ⎧ Rs·Rp·x     x < Rp      (linear through the origin)
ratio(x) =  ⎨ Rs·x²       x < Rs      (quadratic, between plainFreq and sweetFreq)
            ⎩ x³          x ≥ Rs      (cubic; extrapolated above typicalFreq)
```

```
power = typicalPower · ratio(x)                  [W, per core]
cost  = power / ((efficiency/100) · freq_GHz)    [W per relative-GHz]
```

`config/README.md` line 124 confirms the extrapolation ("大于典型频点的功耗使用模型
外插计算"), which is the cubic branch.

**Verification**: all 25 upstream `pwr`/`cost` pairs reproduce to < 0.0015, asserted
in `rust/uperf-config/src/cpu.rs::power_and_cost_match_upstream_log_exactly`. That
test is the highest-value artifact here — the entire governor rests on it.

Notes:
* `efficiency` is Cortex-A53@1.0 GHz = 100, so `efficiency/100 · freq` is relative
  performance, which is what makes `cost` a W-per-performance figure.
* `freeFreq` does **not** appear in the fit. README calls it 单核最低功耗频点 (the
  lowest-power OPP), so it is a floor for capacity reasoning, not a term in the
  curve. It is kept in the struct and flagged as unused by the power curve.
* `nr` (cores per cluster) multiplies power for whole-cluster figures; the
  per-OPP table the binary prints is per core.

## 2. Control path: what actually works on alioth [V]

`scaling_max_freq` is **not writable** on this kernel, even as root:

```
$ ls -laZ /sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq
-r--r--r-- 1 root root u:object_r:sysfs_devices_system_cpu:s0  4096 ... scaling_max_freq
$ echo 1056000 > .../scaling_max_freq
sh: can't create .../scaling_max_freq: Permission denied
```

Mode 444, and the write is refused by the driver, not SELinux (no AVC denial —
`dmesg` is empty and the context is the same for a successful write below).
`scaling_driver` is `qcom-cpufreq-hw` on all three policies. `[ -w ]` reports
"writable" for root because of `CAP_DAC_OVERRIDE`; the *mode bits* are the honest
signal, which is why `RealFs::is_writable` checks mode rather than `access(2)`.

This is exactly why the upstream binary cannot run here:

```
E Tried CpufreqWriterEpicPowersave: Failed to open epic minfreq knob for cluster0
... (all 8 writer strategies)
E Exception thrown: No CpufreqWriter supported for this platform
I Failed to start uperf(pid=23099)
```

Every upstream writer needs a *min*-freq knob; all are locked. **uperf v3 cannot
control CPU frequency on alioth at all.**

What does work [V]:

```
policy0: userspace powersave performance schedutil
$ echo userspace > .../policy4/scaling_governor      # accepted
$ echo 1056000  > .../policy4/scaling_setspeed       # accepted
$ cat .../policy4/scaling_cur_freq                   # 1056000
$ echo 1478400  > .../policy4/scaling_setspeed
$ cat .../policy4/scaling_cur_freq                   # 1478400
$ echo powersave > .../policy4/scaling_governor      # restored
```

So the usable mechanism is `scaling_governor = userspace` + `scaling_setspeed`.
Two consequences worth stating plainly:

* it is a **full takeover** — once a policy is in `userspace` the kernel stops
  scaling it, so the governor must publish a target every cycle (it does);
* if the process dies without disarming, the policy is stuck at the last published
  frequency (mode 444 means nothing else can rescue it). See §4.

`freq_target.rs` resolves per cluster by probing, not by assumption:
`userspace` → per-policy `scaling_max_freq` knob from the config → global
`msm_performance/cpu_max_freq` → none.

## 3. Governor behaviour, verified on device [V]

Reference config `config/sdm888.json`, mode `balance`, scene `idle`
(`slowLimitPower = 1.0 W`, `fastLimitPower = 2.0 W`, `fastLimitCapacity = 15.0`,
`margin = 0.22`, `guideCap` and `limitEfficiency` both true).

Idle:

```
Rust: cpu c0=979200kHz(load 0.00) c1=1574400kHz(load 0.00) c2=1747200kHz(load 0.00) pool=15.00
```

c1/c2 sit above their own margin floor because `limitEfficiency` raises a lower
cluster's cost to match the next cluster's current OPP (documented rule), which is
also why the upstream intent parks them there.

Four spinners pinned to cpu0-3 (`taskset 0f`) — i.e. cluster0 loaded, c1/c2 idle:

```
Rust: cpu c0=1612800kHz(load 1.00) c1=1574400kHz(load 0.00) c2=1747200kHz(load 0.00) pool=0.00
```

and the kernel obeys:

```
policy0 gov=userspace setspeed=1516800 cur=1516800
policy4 gov=userspace setspeed=1574400 cur=1574400
policy7 gov=userspace setspeed=1747200 cur=1632000
```

The cap arithmetic checks out: cluster0 at 1612800 kHz draws 0.216 W/core × 4
cores × load 1.0 = **0.864 W ≤ PL1 1.0 W**, while the next OPP (1708800) would draw
0.257 × 4 = **1.028 W > 1.0 W**. 1612800 is precisely the highest OPP that fits the
cap — the power limit, not the load demand, is the binding constraint, as intended.

Load removed → c0 returns to 883200 kHz and the pool refills (15.00):

```
Rust: cpu c0=883200kHz(load 0.00) ... pool=15.00
```

## 4. Shutdown / disarm [V]

The vendored dfps daemon's `SIGTERM` handler only called `exit()`, leaving the
forked worker alive — and an orphaned worker keeps the governor armed, so the
device stayed in `userspace`. Two fixes in `cpp/uperf/app_main.cpp`:

* the worker's `AppSigHandler` now calls `uperf_rs_stop()` before exiting, which
  disarms and restores the original governors;
* the daemon's `SIGTERM`/`SIGINT` branch now signals the worker(s) and waits 0.5 s
  before exiting, instead of orphaning them.

Verified: `kill -TERM <daemon>` → `superf procs: 0`, all policies back to `powersave`
at their minimum OPPs, log shows

```
Rust: cpu governor disarmed (original governor restored)
uperf_rs_stop: dispatcher joined
```

`SIGKILL` remains unrecoverable in-process (nothing can run after it) — the magisk
module's uninstall path is the place to restore governors for that case. **[U]** not
yet implemented.

## 5. What is approximated, and why

* **Latency smoothing** — README: "能耗代价越大的频点，升频到它的延迟也越大，且低于
  `sweetFreq` 的频点没有额外的升频延迟". Implemented as: move at most one OPP per
  sample, except a `predict`-boosted sample which jumps straight to the goal. The
  README's own text says measured ramp delay always exceeds the configured
  `latencyTime` because sampling is discrete, which is the behaviour this
  reproduces. It is **not** a claim to match upstream tick-for-tick. **[I]**
* **Power-budget allocation** — under the cap, each *loaded* cluster gets the
  highest OPP whose marginal cost (W per relative-GHz) is under a common ceiling,
  and the ceiling is bisected until total power fits the limit. Equalising marginal
  cost is the KKT condition for maximising total capacity subject to a power budget,
  which is what README asks for ("在限定功耗下提供最佳整体性能"), but it is my
  reading rather than a byte-for-byte match. **[I]**
* **Power attribution** — `cluster_power_at_khz × cluster_load`, i.e. only busy
  cores draw. An unconditional idle floor was tried first and, combined with
  `limitEfficiency` parking idle clusters at high OPPs, it ate most of PL1 and
  squeezed a fully loaded cluster0 to 403 kHz — below its own idle target. **[V]**
* **Guide-cap / limit-efficiency capacity table** — implemented from the README
  wording; the exact upstream capacity table is not observable here. **[I]**
* **The OPP candidate set** — upstream printed only a subset of the device OPP
  table on the run that produced §1 (9 of 17 for cluster0). The transcribed c1/c2
  subsets are not consistent with any rule I could fit (not a cost-dedup, not a
  convex hull, not a power threshold), which suggests the capture was mangled.
  This implementation uses the **full** device OPP table, a superset of anything
  upstream could select. The exact upstream filter rule is **[U] unknown**.

## 6. Reproduce

```sh
cd ~/uperf-rewrite
export ANDROID_NDK=~/Android/Sdk/ndk/android-ndk-r30
sh build.sh Release make check          # host tests + device build
cd rust && cargo test --release         # 69 assertions, incl. the 25-OPP golden test
```

On device (alioth `f748d277`):

```sh
adb push build/aarch64-linux-android23/runnable/uperf /data/local/tmp/gov_uperf
adb shell su -c 'mkdir -p /data/local/tmp/gov_test'
adb push config/sdm888.json /data/local/tmp/gov_test_sdm888.json
adb shell su -c 'cp /data/local/tmp/gov_test_sdm888.json /data/local/tmp/gov_test/uperf.json'
adb shell su -c 'nohup /data/local/tmp/gov_uperf /data/local/tmp/gov_test/uperf.json \
    -o /data/local/tmp/gov_test/uperf_log.txt >/dev/null 2>&1 </dev/null &'
# then: taskset 0f sh -c 'while :; do :; done' &  (pin load to cluster0)
grep 'Rust: cpu' /data/local/tmp/gov_test/uperf_log.txt | tail
# stop: kill -TERM $(pidof uperf | cut -d' ' -f1)   -> governors restored
```

Offline (no device): set `UPERF_FAKE_ROOT=/tmp/fake` and the same code writes
`/tmp/fake/sys/devices/system/cpu/cpufreq/policyN/scaling_setspeed` instead —
used by `uperf-core/src/cpu_task.rs::userspace_writer_arms_applies_and_restores`.
