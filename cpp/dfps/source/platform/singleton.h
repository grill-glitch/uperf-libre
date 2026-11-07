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

#include <mutex>

/// Process-wide instance holder.
///
/// The instance is heap-allocated and **deliberately never destroyed**: every T here owns
/// threads, timers and a subscriber table that stay in use until the process exits, and a
/// static-destruction order would have those threads touch already-destroyed mutexes. The
/// destructor of T therefore never runs — say so here rather than leave it implied.
///
/// Two constraints the call sites must respect:
///   * T's constructor must not call GetInstance<T>() (call_once cannot be re-entered on
///     the same flag: that deadlocks). Constructor bodies are kept trivial for this reason.
///   * A throwing constructor leaves the flag unset, so the next call retries and the
///     exception reaches the caller — GetInstance never returns nullptr.
///
/// Construction is enforced at compile time: each T declares its constructor private and
/// friends this class, so `CoBridge b;` cannot create a second, silently independent
/// instance that bypasses the shared state.
template <typename T>
class Singleton {
public:
    Singleton(const Singleton &) = delete;
    Singleton &operator=(const Singleton &) = delete;

    static T *GetInstance(void) {
        std::call_once(once_, []() { instance_ = new T(); });
        return instance_;
    }

protected:
    Singleton() = default;

private:
    static T *instance_;
    static std::once_flag once_;
};

template <typename T>
T *Singleton<T>::instance_ = nullptr;

template <typename T>
std::once_flag Singleton<T>::once_;
