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
#include <condition_variable>
#include <cstddef>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <tuple>
#include <vector>

class HeavyWorker : public Singleton<HeavyWorker> {
public:
    using Handle = int;
    using Work = std::function<void(void)>;

private:
    // Only Singleton<HeavyWorker> may construct it: a second instance would bypass the shared
    // state (the subscriber table / worker slots) the class exists to provide.
    friend class Singleton<HeavyWorker>;
    HeavyWorker();

public:
    ~HeavyWorker();
    Handle Create(const std::string &name);
    void SetWork(Handle handle, const Work &work);
    /// Cancel queued work for `handle` and release the slot (indices are recycled).
    void Destroy(Handle handle);
    /// Stop and join the worker thread. Idempotent.
    void Stop(void);

private:
    std::vector<std::tuple<std::string, Work>> works_;
    /// Slots released by Destroy(), reused by Create().
    std::vector<std::size_t> free_;
    bool pending_;
    bool stop_ = false;
    std::mutex mut_;
    std::condition_variable cv_;
    std::thread th_;
};
