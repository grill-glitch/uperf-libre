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

#include "singleton.h"
#include <atomic>
#include <cstdint>
#include <functional>
#include <mutex>
#include <string>
#include <time.h>
#include <tuple>
#include <vector>

class DelayedWorker : public Singleton<DelayedWorker> {
public:
    using Handle = int;
    using Work = std::function<void(void)>;
    // `ts` is an absolute timestamp in the GetNowTs() clock (microseconds);
    // SLEEP_TS (0) means "no work"/"cancel".
    static constexpr int64_t SLEEP_TS = 0;

private:
    // Only Singleton<DelayedWorker> may construct it: a second instance would bypass the shared
    // state (the subscriber table / worker slots) the class exists to provide.
    friend class Singleton<DelayedWorker>;
    DelayedWorker();

public:
    ~DelayedWorker();
    Handle Create(const std::string &name);
    void SetWork(Handle handle, const Work &work, int64_t ts);
    /// Cancel any pending work for `handle` and release the slot (indices are recycled).
    void Destroy(Handle handle);

    void _OnTimer(void);

private:
    void ReloadTimer(void);
    int FindEarliestWorkIdx(void);
    int FindEarliestReadyWorkIdx(void);

    std::vector<std::tuple<int64_t, Work>> slots_;
    /// Slots released by Destroy(), reused by Create().
    std::vector<std::size_t> free_;
    std::mutex mut_;
    timer_t timerId_;
    // SIGEV_THREAD delivers every expiry on a fresh callback thread, so this one-shot
    // thread-renaming flag is touched concurrently.
    std::atomic<bool> isThreadInited_{false};
};
