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

#include "topapp_monitor.h"
#include "utils/atrace.h"
#include "utils/misc.h"
#include "utils/misc_android.h"
#include <spdlog/spdlog.h>

constexpr char MODULE_NAME[] = "TopappMonitor";
constexpr int64_t TOP_APP_SWITCH_DELAY_MS = 800;
constexpr size_t TOP_TASK_NR_DIFF_MIN = 10;
// Upper bound on how stale `topapp.pkgName` may get. The count-delta gate below is the cheap
// path; a real app switch between two apps with a similar task count never clears it, so the
// probe used to be skipped forever. 2 s bounds that at one `dumpsys` per 2 s.
constexpr int64_t TOP_APP_PROBE_MAX_AGE_MS = 2000;

TopappMonitor::TopappMonitor() : hw_(HwCreate(MODULE_NAME)), dw_(DwCreate(MODULE_NAME)) {}

TopappMonitor::~TopappMonitor() { Stop(); }

void TopappMonitor::Stop(void) {
    // Both queued lambdas capture `this`: cancel them and release the handles, otherwise a
    // late timer/heavy tick writes `prevPkgName_` on freed storage.
    if (dw_ >= 0) {
        DwDestroy(dw_);
        dw_ = -1;
    }
    if (hw_ >= 0) {
        HwDestroy(hw_);
        hw_ = -1;
    }
    ModuleBase::Stop();
}

void TopappMonitor::Start(void) {
    // Subscribe appends without de-duplication: a second Start() would run the handler (and
    // the dumpsys probe) twice per cgroup update.
    if (started_) {
        return;
    }
    started_ = true;
    if (hw_ < 0) {
        hw_ = HwCreate(MODULE_NAME);
    }
    if (dw_ < 0) {
        dw_ = DwCreate(MODULE_NAME);
    }
    using namespace std::placeholders;
    CoSubscribe("cgroup.ta.list", std::bind(&TopappMonitor::OnTopappList, this, _1));
}

void TopappMonitor::OnTopappList(const void *data) {
    if (CoHasSubscriber("topapp.pkgName") == false) {
        return;
    }
    if (data == nullptr) {
        SPDLOG_DEBUG("topapp.pkgName, null pid list");
        return;
    }

    const auto &pl = CoBridge::Get<PidList>(data);
    auto nr = static_cast<int>(pl.size());
    auto now = GetNowTs();
    auto lastProbe = lastProbeTs_.load();
    bool countChanged = std::abs(nr - topappNr_.load()) > static_cast<int>(TOP_TASK_NR_DIFF_MIN);
    bool stale = lastProbe == 0 || now - lastProbe >= MsToUs(TOP_APP_PROBE_MAX_AGE_MS);
    if (!countChanged && !stale) {
        return;
    }
    topappNr_ = nr;
    lastProbeTs_ = now;

    auto delayed = [this]() {
        auto heavywork = [this]() {
            ATRACE_SCOPE(GetTopAppName);
            auto pkgName = GetTopAppNameDumpsys();
            if (pkgName.empty()) {
                return;
            }
            if (pkgName != prevPkgName_) {
                prevPkgName_ = pkgName;
                SPDLOG_DEBUG("topapp.pkgName {}", pkgName);
                // Publish the member, not a local: the pointer handed to subscribers stays
                // valid past this call for anything that keeps it (the contract is still
                // "synchronous", but a stack address would be a use-after-return).
                CoPublish("topapp.pkgName", &prevPkgName_);
            }
        };
        HwSetWork(hw_, heavywork);
    };
    DwSetWork(dw_, delayed, GetNowTs() + MsToUs(TOP_APP_SWITCH_DELAY_MS));
}
