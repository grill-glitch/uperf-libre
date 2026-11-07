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

#include "inotifier.h"
#include "utils/misc.h"
#include <spdlog/spdlog.h>

constexpr char MODULE_NAME[] = "Inotifier";
constexpr int64_t INOTIFY_EVT_MIN_INTERVAL_MS = 10;
// Long enough to keep the CPU idle, short enough that Stop() is prompt.
constexpr int INOTIFY_POLL_TIMEOUT_MS = 100;

Inotifier::Inotifier() {
    running_ = true;
    th_ = std::thread([this]() {
        SetSelfThreadName(MODULE_NAME);
        while (running_) {
            // An exception escaping this thread calls std::terminate and takes the daemon
            // down. A failed read or a throwing notifier must not.
            try {
                if (!inoti_.WaitForEvent(INOTIFY_POLL_TIMEOUT_MS)) {
                    continue;
                }
                inoti_.WaitAndHandle();
            } catch (const std::exception &e) {
                SPDLOG_ERROR("Inotifier: {}", e.what());
                Sleep(MsToUs(1000)); // bound the log rate on a persistent error
            }
            Sleep(MsToUs(INOTIFY_EVT_MIN_INTERVAL_MS));
        }
    });
}

Inotifier::~Inotifier() { Stop(); }

void Inotifier::Stop(void) {
    if (!running_.exchange(false)) {
        return;
    }
    // Joinable, not detached: the loop captures `this`, so a detached thread kept calling
    // into a destroyed Inotify (and closing its fd) when the singleton was ever torn down.
    if (th_.joinable()) {
        th_.join();
    }
}

void Inotifier::Add(const std::string &path, int mask, Inotify::Notifier notifier) {
    return inoti_.Add(path, mask, notifier);
}

bool Inotifier::Remove(const std::string &path) { return inoti_.Remove(path); }
