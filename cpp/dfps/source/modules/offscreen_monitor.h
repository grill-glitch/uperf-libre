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

#pragma once

#include "cobridge_type.h"
#include "platform/module_base.h"
#include <atomic>

class OffscreenMonitor : public ModuleBase {
public:
    OffscreenMonitor();
    ~OffscreenMonitor() override;
    void Start(void) override;
    void Stop(void) override;

    // A registered callback captures `this` and the bridge keeps it for the process
    // lifetime, so copying/moving would leave a second object with different state.
    OffscreenMonitor(const OffscreenMonitor &) = delete;
    OffscreenMonitor &operator=(const OffscreenMonitor &) = delete;

private:
    void OnRestrictedList(const void *data);
    // Written by the publisher thread (cgroup.re.list), read-modify-written for edge
    // detection: atomic so the transition is exactly once.
    std::atomic<bool> prevOffscreen_{false};
    bool started_ = false;
};
