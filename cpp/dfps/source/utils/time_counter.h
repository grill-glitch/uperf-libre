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

#include <atomic>
#include <stdint.h>

/// Elapsed-time helper over the monotonic GetNowTs() clock (microseconds).
///
/// Each call takes a fresh reading, so two consecutive Elapsed*() calls are not guaranteed
/// to agree; ElapsedMs() truncates toward zero (0 for any interval under 1 ms).
/// Instances are safe to share now (start_ is atomic) but Reset()/reads are still only
/// meaningful when driven from one logical event stream.
class TimeCounter {
public:
    TimeCounter();

    void Reset(void);
    int64_t ElapsedUs(void) const;
    int64_t ElapsedMs(void) const;
    double ElapsedS(void) const;

private:
    // Written by Reset() and read by the const Elapsed*() methods: atomic so a counter
    // shared between the producer and consumer thread is not a data race (and so a 32-bit
    // ABI cannot return a torn 64-bit value).
    std::atomic<int64_t> start_{0};
};
