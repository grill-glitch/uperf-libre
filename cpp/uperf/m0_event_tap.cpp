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

M0EventTap::M0EventTap() {
    auto sub = [this](const char *topic, CoBridge::SubscribeCallback cb) {
        CoSubscribe(topic, std::move(cb));
        ++subs_;
    };
    sub("input.touch", [this](const void *d) { OnTouch(d); });
    sub("input.btn", [this](const void *d) { OnBtn(d); });
    sub("input.state", [this](const void *d) { OnInputState(d); });
    sub("topapp.pkgName", [this](const void *d) { OnTopapp(d); });
    sub("offscreen.state", [this](const void *d) { OnOffscreen(d); });
    sub("cgroup.ta.list", [this](const void *d) { OnCgroupList("cgroup.ta.list", d); });
    sub("cgroup.fg.list", [this](const void *d) { OnCgroupList("cgroup.fg.list", d); });
    sub("cgroup.bg.list", [this](const void *d) { OnCgroupList("cgroup.bg.list", d); });
    sub("cgroup.re.list", [this](const void *d) { OnCgroupList("cgroup.re.list", d); });
    sub("cgroup.ta.update", [this](const void *) { OnCgroupUpdate("cgroup.ta.update"); });
    sub("cgroup.fg.update", [this](const void *) { OnCgroupUpdate("cgroup.fg.update"); });
    sub("cgroup.bg.update", [this](const void *) { OnCgroupUpdate("cgroup.bg.update"); });
    sub("cgroup.re.update", [this](const void *) { OnCgroupUpdate("cgroup.re.update"); });
}

M0EventTap::~M0EventTap() { Stop(); }

void M0EventTap::Stop(void) { ModuleBase::Stop(); }

void M0EventTap::Start(void) {
    // Derived from the actual registrations: a literal went stale as soon as a topic was
    // added or removed.
    SPDLOG_INFO("{}: subscribed to {} topics", TAG, subs_);
}

void M0EventTap::OnTouch(const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: input.touch with a null payload", TAG);
        return;
    }
    SPDLOG_INFO("{}: input.touch = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnBtn(const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: input.btn with a null payload", TAG);
        return;
    }
    SPDLOG_INFO("{}: input.btn = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnInputState(const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: input.state with a null payload", TAG);
        return;
    }
    const auto &in = CoBridge::Get<InputData>(data);
    SPDLOG_INFO("{}: input.state = hold:{} swipe:{} gesture:{}", TAG, in.inHold, in.inSwipe, in.inGesture);
}

void M0EventTap::OnTopapp(const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: topapp.pkgName with a null payload", TAG);
        return;
    }
    SPDLOG_INFO("{}: topapp.pkgName = {}", TAG, CoBridge::Get<std::string>(data));
}

void M0EventTap::OnOffscreen(const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: offscreen.state with a null payload", TAG);
        return;
    }
    SPDLOG_INFO("{}: offscreen.state = {}", TAG, CoBridge::Get<bool>(data));
}

void M0EventTap::OnCgroupList(const std::string &topic, const void *data) {
    if (data == nullptr) {
        SPDLOG_WARN("{}: {} with a null payload", TAG, topic);
        return;
    }
    const auto &pl = CoBridge::Get<PidList>(data);
    std::string pids;
    for (size_t i = 0; i < pl.size() && i < 8; i++) {
        pids += std::to_string(pl[i]) + " ";
    }
    SPDLOG_INFO("{}: {} = {} pid(s) [{}]", TAG, topic, pl.size(), pids);
}

void M0EventTap::OnCgroupUpdate(const std::string &topic) {
    // Publisher threads increment this: fetch_add, not `++`.
    auto n = events_.fetch_add(1, std::memory_order_relaxed) + 1;
    if ((n % 64) == 0) {
        SPDLOG_INFO("{}: {} ({} events so far)", TAG, topic, n);
    }
}
