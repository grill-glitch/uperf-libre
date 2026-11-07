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
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

class CoBridge : public Singleton<CoBridge> {
public:
    using SubscribeCallback = std::function<void(const void *)>;
    /// Token returned by Subscribe() and consumed by Unsubscribe(). 0 is "invalid".
    using SubscriptionId = std::uint64_t;

private:
    // Only Singleton<CoBridge> may construct it: a second instance would bypass the shared
    // state (the subscriber table / worker slots) the class exists to provide.
    friend class Singleton<CoBridge>;
    CoBridge();

public:
    // Publish and HasSubscriber are callable from any thread while Subscribe() may run on
    // another (worker threads publish; the FFI thread subscribes): `pubsub_` is guarded.
    void Publish(const std::string &theme, const void *data) const;
    SubscriptionId Subscribe(const std::string &theme, const SubscribeCallback &cb);
    /// Remove one registration. Without this a callback bound to a destroyed object stayed
    /// in the table for the whole process lifetime and fired on freed memory.
    bool Unsubscribe(const std::string &theme, SubscriptionId id);
    bool HasSubscriber(const std::string &theme) const;

    // `data` is an untyped pointer owned by the publisher. It is only guaranteed valid for
    // the duration of the synchronous Publish() call, and it may be null for topics whose
    // payload is the event itself (e.g. `cgroup.*.update`) — check before dereferencing.
    template <typename T>
    static const T &Get(const void *data) {
        return *reinterpret_cast<const T *>(data);
    }

private:
    using Entry = std::pair<SubscriptionId, SubscribeCallback>;
    /// Copy-on-write per topic: publishing is the hot path (every input/cgroup event) and
    /// used to copy the whole callback list on each call just to release the lock. Now
    /// Publish() only bumps a refcount, and Subscribe()/Unsubscribe() (rare) build a new
    /// list, so a callback that re-enters them can never invalidate a running dispatch.
    using EntryList = std::shared_ptr<const std::vector<Entry>>;
    mutable std::mutex mutex_;
    SubscriptionId nextId_ = 0;
    std::unordered_map<std::string, EntryList> pubsub_;
};
