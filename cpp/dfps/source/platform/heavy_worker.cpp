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

#include "heavy_worker.h"
#include "utils/fmt_exception.h"
#include "utils/misc.h"
#include <climits>
#include <spdlog/spdlog.h>

constexpr char MODULE_NAME[] = "HeavyWorker";

HeavyWorker::HeavyWorker() : pending_(false) {
    th_ = std::thread([this]() mutable {
        SetSelfThreadName(MODULE_NAME);
        for (;;) {
            size_t count = 0;
            {
                std::unique_lock<std::mutex> lk(mut_);
                cv_.wait(lk, [this]() { return pending_ || stop_; });
                if (stop_) {
                    return;
                }
                pending_ = false;
                // Snapshot the slot count: `Create()` may append while this pass runs, and
                // iterating the vector by reference used to walk invalidated iterators.
                count = works_.size();
            }
            for (size_t i = 0; i < count; ++i) {
                Work work;
                {
                    std::lock_guard<std::mutex> lk(mut_);
                    if (i >= works_.size()) {
                        break;
                    }
                    auto &w = std::get<1>(works_[i]);
                    if (w == nullptr) {
                        continue;
                    }
                    work = w;
                    w = nullptr;
                }
                // An exception escaping the worker thread calls std::terminate.
                try {
                    work();
                } catch (const std::exception &e) {
                    SPDLOG_ERROR("HeavyWorker task failed: {}", e.what());
                } catch (...) {
                    SPDLOG_ERROR("HeavyWorker task failed with an unknown exception");
                }
            }
        }
    });
}

HeavyWorker::~HeavyWorker() { Stop(); }

void HeavyWorker::Stop(void) {
    {
        std::lock_guard<std::mutex> lk(mut_);
        stop_ = true;
    }
    cv_.notify_all();
    // Joinable, not detached: a detached thread could touch `mut_`/`cv_`/`works_` after
    // this object is gone, and there was no way to shut the worker down at all.
    if (th_.joinable()) {
        th_.join();
    }
}

HeavyWorker::Handle HeavyWorker::Create(const std::string &name) {
    std::lock_guard<std::mutex> lk(mut_);
    if (!free_.empty()) {
        auto handle = static_cast<Handle>(free_.back());
        free_.pop_back();
        works_[static_cast<std::size_t>(handle)] = std::make_tuple(name, Work());
        return handle;
    }
    if (works_.size() >= static_cast<std::size_t>(INT_MAX)) {
        throw FmtException("HeavyWorker slot table is full");
    }
    Handle handle = static_cast<Handle>(works_.size());
    works_.emplace_back(name, nullptr);
    return handle;
}

void HeavyWorker::SetWork(Handle handle, const Work &work) {
    std::unique_lock<std::mutex> lk(mut_);
    // Validate under the lock: Create() grows the vector on other threads.
    if (handle < 0 || static_cast<std::size_t>(handle) >= works_.size()) {
        throw FmtException("HeavyWorker handle not found, handle = {}", handle);
    }

    auto &w = std::get<1>(works_[handle]);
    w = work;
    if (work) {
        pending_ = true;
        lk.unlock();
        cv_.notify_one();
    }
}

void HeavyWorker::Destroy(Handle handle) {
    std::lock_guard<std::mutex> lk(mut_);
    if (handle < 0 || static_cast<std::size_t>(handle) >= works_.size()) {
        return;
    }
    auto &entry = works_[static_cast<std::size_t>(handle)];
    std::get<1>(entry) = nullptr;
    std::get<0>(entry).clear();
    // Keep the index for reuse (a hole would otherwise make handles unbounded), and do not
    // push it twice if Destroy() is called again for the same handle.
    for (auto i : free_) {
        if (i == static_cast<std::size_t>(handle)) {
            return;
        }
    }
    free_.push_back(static_cast<std::size_t>(handle));
}
