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
#include "utils/time_counter.h"
#include <atomic>
#include <string>

class TopappMonitor : public ModuleBase {
public:
    TopappMonitor();
    ~TopappMonitor() override;
    void Start(void) override;
    /// Cancel the queued dumpsys probe and release the worker handles.
    void Stop(void) override;

    // The registered callbacks capture `this` and live for the process lifetime.
    TopappMonitor(const TopappMonitor &) = delete;
    TopappMonitor &operator=(const TopappMonitor &) = delete;

private:
    void OnTopappList(const void *data);

    // Written from the publishing thread (cgroup.ta.list) and read on the same path when
    // the next event arrives: atomic, because those publishes are not guaranteed to be on
    // one thread.
    std::atomic<int> topappNr_{0};
    /// GetNowTs() of the last dumpsys probe (0 = never).
    std::atomic<int64_t> lastProbeTs_{0};
    std::string prevPkgName_;
    HeavyWorker::Handle hw_;
    DelayedWorker::Handle dw_;
    bool started_ = false;
};
