/*
 * Copyright (C) 2021-2022 Matt Yang
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

#include "offscreen_monitor.h"
#include "utils/misc.h"
#include "utils/misc_android.h"
#include <spdlog/spdlog.h>

// constexpr char MODULE_NAME[] = "OffscreenMonitor";
constexpr int RESTRICTED_TASK_NR_MIN = 10;

OffscreenMonitor::OffscreenMonitor() {}

OffscreenMonitor::~OffscreenMonitor() { Stop(); }

void OffscreenMonitor::Stop(void) {
    started_ = false;
    ModuleBase::Stop();
}

void OffscreenMonitor::Start(void) {
    // Subscribe appends without de-duplication: a second Start() would run the handler
    // twice per cgroup update.
    if (started_) {
        return;
    }
    started_ = true;
    using namespace std::placeholders;
    CoSubscribe("cgroup.re.list", std::bind(&OffscreenMonitor::OnRestrictedList, this, _1));
}

void OffscreenMonitor::OnRestrictedList(const void *data) {
    // The payload is an untyped pointer: a null/malformed publish would otherwise
    // dereference null inside CoBridge::Get.
    if (data == nullptr) {
        SPDLOG_DEBUG("offscreen.state, null pid list");
        return;
    }
    // Reading the restricted cgroup costs 3~8 ms per scan; do not pay it unless someone
    // consumes the result.
    if (CoHasSubscriber("offscreen.state") == false) {
        return;
    }
    const auto &pids = CoBridge::Get<PidList>(data);
    bool isOffscreen = pids.size() > static_cast<size_t>(RESTRICTED_TASK_NR_MIN);
    // exchange() makes the read-modify-write one step: overlapping timer threads cannot
    // both publish the same transition.
    if (prevOffscreen_.exchange(isOffscreen) != isOffscreen) {
        SPDLOG_DEBUG("offscreen.state {}", isOffscreen);
        CoPublish("offscreen.state", &isOffscreen);
    }
}
