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

#include "platform/module_base.h"

ModuleBase::ModuleBase() {}

ModuleBase::~ModuleBase() {
    // A derived destructor has already run, so virtual dispatch is gone; this cleanup must
    // be unconditional and base-local. Without it every module left `this`-bound callbacks
    // in the process-wide bridge for good, and the next publish called into freed memory.
    Stop();
}

void ModuleBase::Stop(void) {
    for (const auto &entry : subs_) {
        CoBridge::GetInstance()->Unsubscribe(entry.first, entry.second);
    }
    subs_.clear();
}

void ModuleBase::CoPublish(const std::string &theme, const void *data) const {
    return CoBridge::GetInstance()->Publish(theme, data);
}

CoBridge::SubscriptionId ModuleBase::CoSubscribe(const std::string &theme, const CoBridge::SubscribeCallback &cb) {
    auto id = CoBridge::GetInstance()->Subscribe(theme, cb);
    if (id != 0) {
        subs_.emplace_back(theme, id);
    }
    return id;
}

bool ModuleBase::CoHasSubscriber(const std::string &theme) const {
    return CoBridge::GetInstance()->HasSubscriber(theme);
}

DelayedWorker::Handle ModuleBase::DwCreate(const std::string &name) {
    return DelayedWorker::GetInstance()->Create(name);
}

void ModuleBase::DwSetWork(DelayedWorker::Handle handle, const DelayedWorker::Work &work, int64_t ts) {
    return DelayedWorker::GetInstance()->SetWork(handle, work, ts);
}

void ModuleBase::DwDestroy(DelayedWorker::Handle handle) { DelayedWorker::GetInstance()->Destroy(handle); }

HeavyWorker::Handle ModuleBase::HwCreate(const std::string &name) { return HeavyWorker::GetInstance()->Create(name); }

void ModuleBase::HwSetWork(HeavyWorker::Handle handle, const HeavyWorker::Work &work) {
    return HeavyWorker::GetInstance()->SetWork(handle, work);
}

void ModuleBase::HwDestroy(HeavyWorker::Handle handle) { HeavyWorker::GetInstance()->Destroy(handle); }
