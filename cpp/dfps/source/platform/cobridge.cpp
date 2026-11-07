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

#include "cobridge.h"

CoBridge::CoBridge() {}

void CoBridge::Publish(const std::string &theme, const void *data) const {
    // Take the immutable snapshot by reference count, then invoke outside the lock: a
    // callback may call Subscribe()/Unsubscribe() (which take the same lock and publish a
    // *new* list), so neither the lock nor the dispatched list can be invalidated.
    EntryList snapshot;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = pubsub_.find(theme);
        if (it == pubsub_.end()) {
            return;
        }
        snapshot = it->second;
    }
    if (!snapshot) {
        return;
    }
    for (const auto &entry : *snapshot) {
        if (entry.second != nullptr) {
            entry.second(data);
        }
    }
}

CoBridge::SubscriptionId CoBridge::Subscribe(const std::string &theme, const SubscribeCallback &cb) {
    if (cb == nullptr) {
        return 0;
    }

    std::lock_guard<std::mutex> lock(mutex_);
    auto id = ++nextId_;
    auto &slot = pubsub_[theme];
    // Build the new list outside any reader's view: a concurrent Publish() keeps using the
    // previous immutable vector.
    auto next = slot ? std::make_shared<std::vector<Entry>>(*slot)
                     : std::make_shared<std::vector<Entry>>();
    next->emplace_back(id, cb);
    slot = std::move(next);
    return id;
}

bool CoBridge::Unsubscribe(const std::string &theme, SubscriptionId id) {
    if (id == 0) {
        return false;
    }
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = pubsub_.find(theme);
    if (it == pubsub_.end()) {
        return false;
    }
    const auto &entries = it->second;
    if (!entries) {
        return false;
    }
    auto remaining = std::make_shared<std::vector<Entry>>();
    remaining->reserve(entries->size());
    bool found = false;
    for (const auto &e : *entries) {
        if (e.first == id) {
            found = true;
            continue;
        }
        remaining->push_back(e);
    }
    if (!found) {
        return false;
    }
    if (remaining->empty()) {
        // Drop the topic entry too, so HasSubscriber() and the map do not keep growing.
        pubsub_.erase(it);
    } else {
        it->second = std::move(remaining);
    }
    return true;
}

bool CoBridge::HasSubscriber(const std::string &theme) const {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = pubsub_.find(theme);
    return it != pubsub_.end() && it->second && !it->second->empty();
}
