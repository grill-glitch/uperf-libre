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

#include "m0_event_tap.h"
#include "modules/cobridge_type.h"
#include <spdlog/spdlog.h>

// NOTE (M1): the payloads below are the raw C++ objects the platform publishes
// (`std::string*`, `PidList*` = `std::vector<int>*`). Dereferencing them here is fine
// because this tap is C++ and lives inside the same STL ABI. The Rust side must NOT do
// this — cpp/uperf/bridge.cpp converts each payload to a C-layout struct first
// (see AGENT.md §5.1).
static constexpr const char *TAG = "EventTap";

M0EventTap::M0EventTap() : events_(0) {
    CoSubscribe("input.touch", [this](const void *d) { OnTouch(d); });
    CoSubscribe("input.btn", [this](const void *d) { OnBtn(d); });
    CoSubscribe("input.state", [this](const void *d) { OnInputState(d); });
    CoSubscribe("topapp.pkgName", [this](const void *d) { OnTopapp(d); });
    CoSubscribe("offscreen.state", [this](const void *d) { OnOffscreen(d); });
    CoSubscribe("cgroup.ta.list", [this](const void *d) { OnCgroupList("cgroup.ta.list", d); });
    CoSubscribe("cgroup.fg.list", [this](const void *d) { OnCgroupList("cgroup.fg.list", d); });
    CoSubscribe("cgroup.bg.list", [this](const void *d) { OnCgroupList("cgroup.bg.list", d); });
    CoSubscribe("cgroup.re.list", [this](const void *d) { OnCgroupList("cgroup.re.list", d); });
    CoSubscribe("cgroup.ta.update", [this](const void *d) { OnCgroupUpdate("cgroup.ta.update"); });
    CoSubscribe("cgroup.fg.update", [this](const void *d) { OnCgroupUpdate("cgroup.fg.update"); });
    CoSubscribe("cgroup.bg.update", [this](const void *d) { OnCgroupUpdate("cgroup.bg.update"); });
    CoSubscribe("cgroup.re.update", [this](const void *d) { OnCgroupUpdate("cgroup.re.update"); });
}

M0EventTap::~M0EventTap() {}

void M0EventTap::Start(void) {
    SPDLOG_INFO("{}: subscribed to 13 topics", TAG);
}

void M0EventTap::OnTouch(const void *data) {
    SPDLOG_INFO("{}: input.touch = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnBtn(const void *data) {
    SPDLOG_INFO("{}: input.btn = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnInputState(const void *data) {
    const auto &in = CoBridge::Get<InputData>(data);
    SPDLOG_INFO("{}: input.state = hold:{} swipe:{} gesture:{}", TAG, in.inHold, in.inSwipe, in.inGesture);
}

void M0EventTap::OnTopapp(const void *data) {
    SPDLOG_INFO("{}: topapp.pkgName = {}", TAG, CoBridge::Get<std::string>(data));
}

void M0EventTap::OnOffscreen(const void *data) {
    SPDLOG_INFO("{}: offscreen.state = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnCgroupList(const char *topic, const void *data) {
    const auto &pl = CoBridge::Get<PidList>(data);
    std::string pids;
    for (size_t i = 0; i < pl.size() && i < 8; i++) {
        pids += std::to_string(pl[i]) + " ";
    }
    SPDLOG_INFO("{}: {} = {} pid(s) [{}]", TAG, topic, pl.size(), pids);
}

void M0EventTap::OnCgroupUpdate(const char *topic) {
    if ((++events_ % 64) == 0) {
        SPDLOG_INFO("{}: {} ({} events so far)", TAG, topic, events_);
    }
}
