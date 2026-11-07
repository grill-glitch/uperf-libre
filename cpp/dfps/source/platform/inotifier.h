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
#include "utils/inotify.h"
#include <atomic>
#include <string>
#include <thread>

class Inotifier : public Singleton<Inotifier> {
public:
private:
    // Only Singleton<Inotifier> may construct it: a second instance would bypass the shared
    // state (the subscriber table / worker slots) the class exists to provide.
    friend class Singleton<Inotifier>;
    Inotifier();

public:
    ~Inotifier();
    /// Register a watch. Thread-safe (Add may run while the watcher thread is dispatching).
    void Add(const std::string &path, int mask, Inotify::Notifier notifier);
    /// Remove the watch on `path`, so a callback bound to a module instance cannot fire
    /// after that module is destroyed.
    bool Remove(const std::string &path);
    /// Stop and join the watcher thread. Idempotent.
    void Stop(void);

private:
    Inotify inoti_;
    std::thread th_;
    std::atomic<bool> running_{false};
};
