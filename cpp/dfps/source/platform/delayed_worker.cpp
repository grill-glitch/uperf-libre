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

#include "delayed_worker.h"
#include "utils/fmt_exception.h"
#include "utils/misc.h"
#include <cerrno>
#include <climits>
#include <signal.h>
#include <spdlog/spdlog.h>
#include <unistd.h>

constexpr char MODULE_NAME[] = "DelayedWorker";

void OnTimerWrapper(union sigval v) { DelayedWorker::GetInstance()->_OnTimer(); }

DelayedWorker::DelayedWorker() : timerId_(nullptr) {
    constexpr int SIVAL_INT_DEFAULT = 111;
    struct sigevent evp;
    memset(&evp, 0, sizeof(struct sigevent));
    evp.sigev_value.sival_int = SIVAL_INT_DEFAULT;
    evp.sigev_notify = SIGEV_THREAD;
    evp.sigev_notify_function = OnTimerWrapper;
    // An unchecked failure leaves timerId_ invalid: every later timer_settime() then fails
    // silently and the scheduler simply never fires.
    if (timer_create(CLOCK_MONOTONIC, &evp, &timerId_) != 0) {
        throw FmtException("timer_create failed, errno = {}", errno);
    }
}

DelayedWorker::~DelayedWorker() { timer_delete(timerId_); }

DelayedWorker::Handle DelayedWorker::Create(const std::string &name) {
    std::lock_guard<std::mutex> lk(mut_);
    if (!free_.empty()) {
        auto handle = static_cast<Handle>(free_.back());
        free_.pop_back();
        slots_[static_cast<std::size_t>(handle)] = std::make_tuple(SLEEP_TS, Work());
        return handle;
    }
    if (slots_.size() >= static_cast<std::size_t>(INT_MAX)) {
        throw FmtException("DelayedWorker slot table is full");
    }
    Handle handle = static_cast<Handle>(slots_.size());
    slots_.emplace_back(std::make_tuple(SLEEP_TS, nullptr));
    return handle;
}

void DelayedWorker::Destroy(Handle handle) {
    std::lock_guard<std::mutex> lk(mut_);
    if (handle < 0 || static_cast<std::size_t>(handle) >= slots_.size()) {
        return;
    }
    auto &[preserveTs, w] = slots_[static_cast<std::size_t>(handle)];
    // Cancelling the work is what makes the module teardown safe: a queued lambda that
    // captured `this` must not run after the object is gone.
    w = nullptr;
    preserveTs = SLEEP_TS;
    for (auto i : free_) {
        if (i == static_cast<std::size_t>(handle)) {
            ReloadTimer();
            return;
        }
    }
    free_.push_back(static_cast<std::size_t>(handle));
    ReloadTimer();
}

void DelayedWorker::SetWork(Handle handle, const Work &work, int64_t ts) {
    std::lock_guard<std::mutex> lk(mut_);
    // Validate under the lock: `slots_.size()` is otherwise read while Create() may be
    // appending on another thread (a TOCTOU check that protects nothing).
    if (handle < 0 || static_cast<std::size_t>(handle) >= slots_.size()) {
        throw FmtException("DelayedWorker handle not found, handle = {}", handle);
    }

    auto &[preserveTs, w] = slots_[handle];
    w = work;
    preserveTs = ts;
    if (work == nullptr) {
        preserveTs = SLEEP_TS;
    }
    ReloadTimer();
}

void DelayedWorker::_OnTimer(void) {
    if (!isThreadInited_.exchange(true)) {
        SetSelfThreadName(MODULE_NAME);
    }

    Work work;
    {
        std::lock_guard<std::mutex> lk(mut_);
        auto idx = FindEarliestReadyWorkIdx();
        if (idx >= 0) {
            auto &[preserveTs, w] = slots_[idx];
            preserveTs = SLEEP_TS;
            if (w) {
                work = w;
            }
        }
    }

    // SetWork() may be called in work(). A throwing work must never escape this
    // SIGEV_THREAD callback: that calls std::terminate, and it would also skip the
    // re-arm below, wedging every scheduled task.
    if (work) {
        try {
            work();
        } catch (const std::exception &e) {
            SPDLOG_ERROR("DelayedWorker task failed: {}", e.what());
        } catch (...) {
            SPDLOG_ERROR("DelayedWorker task failed with an unknown exception");
        }
    }

    std::lock_guard<std::mutex> lk(mut_);
    ReloadTimer();
}

void DelayedWorker::ReloadTimer(void) {
    constexpr int64_t MIN_INTERVAL = 1;
    auto setTimer = [this](int64_t usec) {
        struct itimerspec itv;
        memset(&itv, 0, sizeof(itv));
        // Signed arithmetic: mixing in an `ull` literal promotes `usec` to unsigned.
        itv.it_value.tv_sec = usec / 1000000LL;
        itv.it_value.tv_nsec = (usec % 1000000LL) * 1000LL;
        if (timer_settime(timerId_, 0, &itv, nullptr) != 0) {
            // A broken timer silently stops the whole scheduler: say so once per failure.
            SPDLOG_ERROR("DelayedWorker: timer_settime failed, errno = {}", errno);
        }
    };

    auto idx = FindEarliestWorkIdx();
    if (idx >= 0) {
        // if preserveTs is earlier than now, work also need to be done
        auto now = GetNowTs();
        const auto &[preserveTs, w] = slots_[idx];
        auto delta = (preserveTs > now) ? (preserveTs - now) : MIN_INTERVAL;
        setTimer(delta);
    } else {
        setTimer(0);
    }
}

int DelayedWorker::FindEarliestWorkIdx(void) {
    int64_t nextTs = INT64_MAX;
    int idx = -1;
    for (std::size_t i = 0; i < slots_.size(); ++i) {
        const auto &[preserveTs, w] = slots_[i];
        if (preserveTs > SLEEP_TS && preserveTs < nextTs) {
            nextTs = preserveTs;
            idx = i;
        }
    }
    return idx;
}

int DelayedWorker::FindEarliestReadyWorkIdx(void) {
    auto now = GetNowTs();
    int64_t nextTs = INT64_MAX;
    int idx = -1;
    for (std::size_t i = 0; i < slots_.size(); ++i) {
        const auto &[preserveTs, w] = slots_[i];
        if (preserveTs > SLEEP_TS && preserveTs < nextTs && preserveTs < now) {
            nextTs = preserveTs;
            idx = i;
        }
    }
    return idx;
}
