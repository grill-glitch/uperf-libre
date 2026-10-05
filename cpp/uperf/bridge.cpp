/*
 * Copyright (C) 2026 grill-glitch
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// C++ ↔ Rust bridge (AGENT.md §5).
//
// This file owns the entire C ABI surface declared in `cpp/include/uperf_rs.h`:
//   * `uperf_bridge_subscribe(topic)` — Rust calls to install a callback for a topic.
//   * `uperf_bridge_write_log(msg, len)` — Rust calls to emit a log line via spdlog.
//   * `uperf_rs_on_event(...)` — C++ invokes when the platform publishes a topic.
//
// Payload conversion rule (AGENT.md §5.1): Rust never sees C++ objects directly.
// `dispatch()` converts every C++ payload into the C-layout struct the Rust
// engine expects, then calls `uperf_rs_on_event`.

#include "uperf_rs_bridge.h"
#include "modules/cobridge_type.h"
#include "platform/cobridge.h"
#include "platform/module_base.h"

#include <spdlog/spdlog.h>

#include <cstring>
#include <string>
#include <vector>

extern "C" {
void uperf_rs_on_event(const char *topic, const void *data, size_t len);
int  uperf_rs_start(const char *config_path, const char *log_path);
void uperf_rs_reload(void);
void uperf_rs_stop(void);
void uperf_rs_init(const uperf_bridge_t *bridge);
}

// ---------------------------------------------------------------------------
//  Rust → C++ log sink: forward bytes to spdlog at INFO level
// ---------------------------------------------------------------------------

static std::string log_prefix; // reserved for future per-tag routing

// `uperf_bridge_write_log`: forward bytes to spdlog at INFO level.
// The original v3 binary's spdlog pattern is "%H:%M:%S %L %v" (no logger name) —
// keep the same. The argument identifies the originating Rust module so we can
// route per-tag (e.g. "Config", "SfAnalysis", "Switcher") once M2 wires the
// schema-specific code. Tag "" means default and matches the original's bare
// output.
extern "C" void uperf_bridge_write_log(const char *tag, const char *msg, size_t len) {
    if (msg == nullptr || len == 0) {
        return;
    }
    std::string line(msg, msg + len);
    while (!line.empty() && (line.back() == '\n' || line.back() == '\r')) {
        line.pop_back();
    }
    if (tag == nullptr || *tag == '\0') {
        SPDLOG_INFO("{}", line);
    } else {
        // spdlog default pattern (%v) does NOT include %n; we prefix the tag
        // manually so the original's grep-by-class-name workflow keeps working.
        SPDLOG_INFO("[{}] {}", tag, line);
    }
}

// ---------------------------------------------------------------------------
//  C++ → Rust: topic subscriptions
//
//  We register a CoBridge subscriber for every topic Rust cares about. When the
//  platform publishes, `dispatch()` converts the payload to a C struct and
//  forwards it across the FFI boundary.
// ---------------------------------------------------------------------------

namespace {

// Output payloads live in these per-call scratch buffers; they are valid for the
// duration of `uperf_rs_on_event` (AGENT.md §5.1 lifetime rule).

struct PidListBuf {
    std::vector<int32_t> storage;
};

static thread_local PidListBuf pid_buf;

void publish_touch(const std::string &topic, const void *data) {
    bool v = CoBridge::Get<bool>(data);
    int32_t out = v ? 1 : 0;
    uperf_rs_on_event(topic.c_str(), &out, sizeof(out));
}

void publish_btn(const std::string &topic, const void *data) {
    bool v = CoBridge::Get<bool>(data);
    int32_t out = v ? 1 : 0;
    uperf_rs_on_event(topic.c_str(), &out, sizeof(out));
}

void publish_input_state(const std::string &topic, const void *data) {
    const InputData &in = CoBridge::Get<InputData>(data);
    int32_t s[3] = {
        in.inHold ? 1 : 0,
        in.inSwipe ? 1 : 0,
        in.inGesture ? 1 : 0,
    };
    uperf_rs_on_event(topic.c_str(), s, sizeof(s));
}

void publish_topapp(const std::string &topic, const void *data) {
    const std::string &pkg = CoBridge::Get<std::string>(data);
    uperf_rs_on_event(topic.c_str(), pkg.c_str(), pkg.size());
}

void publish_offscreen(const std::string &topic, const void *data) {
    bool v = CoBridge::Get<bool>(data);
    int32_t out = v ? 1 : 0;
    uperf_rs_on_event(topic.c_str(), &out, sizeof(out));
}

void publish_cgroup_list(const std::string &topic, const void *data) {
    const PidList &pl = CoBridge::Get<PidList>(data);
    pid_buf.storage.assign(pl.begin(), pl.end());
    struct {
        const int32_t *pids;
        size_t len;
    } payload{pid_buf.storage.data(), pid_buf.storage.size()};
    uperf_rs_on_event(topic.c_str(), &payload, sizeof(payload));
}

void publish_cgroup_update(const std::string &topic) {
    uperf_rs_on_event(topic.c_str(), nullptr, 0);
}

void install_subscribers(void) {
    auto bridge = CoBridge::GetInstance();

    bridge->Subscribe("input.touch",
                      [](const void *d) { publish_touch("input.touch", d); });
    bridge->Subscribe("input.btn",
                      [](const void *d) { publish_btn("input.btn", d); });
    bridge->Subscribe("input.state",
                      [](const void *d) { publish_input_state("input.state", d); });
    bridge->Subscribe("topapp.pkgName",
                      [](const void *d) { publish_topapp("topapp.pkgName", d); });
    bridge->Subscribe("offscreen.state",
                      [](const void *d) { publish_offscreen("offscreen.state", d); });
    bridge->Subscribe("cgroup.ta.list",
                      [](const void *d) { publish_cgroup_list("cgroup.ta.list", d); });
    bridge->Subscribe("cgroup.fg.list",
                      [](const void *d) { publish_cgroup_list("cgroup.fg.list", d); });
    bridge->Subscribe("cgroup.bg.list",
                      [](const void *d) { publish_cgroup_list("cgroup.bg.list", d); });
    bridge->Subscribe("cgroup.re.list",
                      [](const void *d) { publish_cgroup_list("cgroup.re.list", d); });
    bridge->Subscribe("cgroup.ta.update",
                      [](const void *) { publish_cgroup_update("cgroup.ta.update"); });
    bridge->Subscribe("cgroup.fg.update",
                      [](const void *) { publish_cgroup_update("cgroup.fg.update"); });
    bridge->Subscribe("cgroup.bg.update",
                      [](const void *) { publish_cgroup_update("cgroup.bg.update"); });
    bridge->Subscribe("cgroup.re.update",
                      [](const void *) { publish_cgroup_update("cgroup.re.update"); });
}

// `uperf_bridge_subscribe` is a Rust-side helper: it walks the known topic list
// and asks CoBridge to install the corresponding C++ publisher. The Rust side
// never sees C++ objects — `subscribe` here just records "Rust is interested"
// and the dispatcher forwards every event we install above.

int g_subscribers_installed = 0;

} // namespace

extern "C" int uperf_bridge_subscribe(const char *topic) {
    if (topic == nullptr) {
        return -1;
    }
    if (g_subscribers_installed == 0) {
        install_subscribers();
        g_subscribers_installed = 1;
    }
    return 0;
}

// ---------------------------------------------------------------------------
//  App-side bridge handle given to Rust at `uperf_rs_init`
//
//  This is the table the Rust side stores in `BRIDGE`. The C++ side does not
//  store anything back — Rust is the consumer only.
// ---------------------------------------------------------------------------

namespace {
extern "C" int bridge_subscribe(const char *topic) {
    return uperf_bridge_subscribe(topic);
}
extern "C" void bridge_write_log(const char *tag, const char *msg, size_t len) {
    uperf_bridge_write_log(tag, msg, len);
}
const uperf_bridge_t kBridge = {bridge_subscribe, bridge_write_log};
} // namespace

extern "C" const uperf_bridge_t *uperf_bridge_handle(void) {
    return &kBridge;
}

// ---------------------------------------------------------------------------
//  uperf_rs_init wrapper called from app_main.cpp
// ---------------------------------------------------------------------------

extern "C" void uperf_bridge_init_rust(const char *config_path, const char *log_path) {
    uperf_rs_init(&kBridge);
    if (uperf_rs_start(config_path, log_path) != 0) {
        SPDLOG_ERROR("uperf_rs_start failed");
    }
}