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

#include <stdbool.h>
#include <stdlib.h>
#include <sys/cdefs.h>

__BEGIN_DECLS

int AtraceInit(void);
bool AtraceIsEnable(void);
void AtraceToggle(bool enable);
void AtraceCntWrite(const char *tag, long long cnt);
void AtraceScopeBegin(const char *tag);
void AtraceScopeEnd(void);

// `do { } while (0)`: a bare block breaks `if (c) ATRACE_NAME(x); else ...`.
// The per-site counter is atomic: a function instrumented from several threads would
// otherwise read-modify-write it unsynchronised (UB, lost/duplicated counts).
#define ATRACE_NAME(name)                                                                                              \
    do {                                                                                                               \
        static unsigned long long cnt = 0;                                                                             \
        if (AtraceIsEnable()) {                                                                                        \
            unsigned long long c = __atomic_fetch_add(&cnt, 1, __ATOMIC_RELAXED);                                       \
            AtraceCntWrite(name, (long long)(c & 0x1));                                                                \
        }                                                                                                              \
    } while (0)

#define ATRACE_CALL() ATRACE_NAME(__func__)

__END_DECLS

#ifdef __cplusplus

#include <string>
#include <string_view>

// RAII guard: its destructor emits the matching `E|` marker, so a copy would emit it
// twice and corrupt the trace stack.
struct AtraceScopeHelper {
    explicit AtraceScopeHelper(const char *tag) { AtraceScopeBegin(tag); }
    // std::string_view::data() is not guaranteed to be NUL-terminated, and the C side
    // passes it to vsnprintf("%s").
    explicit AtraceScopeHelper(const std::string &tag) { AtraceScopeBegin(tag.c_str()); }
    AtraceScopeHelper(const AtraceScopeHelper &) = delete;
    AtraceScopeHelper &operator=(const AtraceScopeHelper &) = delete;
    ~AtraceScopeHelper() { AtraceScopeEnd(); }
};

// Unique per expansion: two instrumentation points in one scope used to collide on `__ash`.
#define ATRACE_CONCAT_(a, b) a##b
#define ATRACE_CONCAT(a, b) ATRACE_CONCAT_(a, b)
#define ATRACE_SCOPE_FUNC() AtraceScopeHelper ATRACE_CONCAT(atrace_scope_, __LINE__)(__PRETTY_FUNCTION__)
#define ATRACE_SCOPE(name) AtraceScopeHelper ATRACE_CONCAT(atrace_scope_, __LINE__)(#name)

#endif
