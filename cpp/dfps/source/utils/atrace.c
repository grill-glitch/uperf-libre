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

#include "atrace.h"
#include <errno.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stdarg.h>
#include <stdio.h>
#include <unistd.h>

// Both are touched from the bridge (Rust FFI thread) and from whichever thread emits a
// trace line, so plain bool/int would be a C data race.
static atomic_bool atrace_enable;
// -1, not 0: descriptor 0 is a perfectly valid fd.
static atomic_int tracemark_fd = -1;
static int self_pid;

static int AtraceFd(void) { return atomic_load(&tracemark_fd); }

// Write the whole buffer, retrying on EINTR, and never hand `write()` a length that the
// buffer does not actually hold.
static void AtraceWrite(const char *buf, int len) {
    int fd = AtraceFd();
    if (fd < 0 || len <= 0) {
        return;
    }
    int off = 0;
    while (off < len) {
        ssize_t n = write(fd, buf + off, (size_t)(len - off));
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return;
        }
        off += (int)n;
    }
}

// snprintf() returns the length the string *would* need, so a truncated (or negative)
// result must be clamped before it reaches write().
static void AtraceEmit(const char *fmt, ...) {
    if (!AtraceIsEnable()) {
        return;
    }
    char buf[128];
    va_list ap;
    va_start(ap, fmt);
    int len = vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    if (len <= 0) {
        return;
    }
    if (len >= (int)sizeof(buf)) {
        len = (int)sizeof(buf) - 1;
    }
    AtraceWrite(buf, len);
}

int AtraceInit(void) {
    atomic_store(&atrace_enable, false);
    self_pid = getpid();

    int old = atomic_load(&tracemark_fd);
    if (old >= 0) {
        close(old);
        atomic_store(&tracemark_fd, -1);
    }

    int fd = open("/sys/kernel/debug/tracing/trace_marker", O_WRONLY | O_CLOEXEC);
    if (fd < 0) {
        fd = open("/sys/kernel/tracing/trace_marker", O_WRONLY | O_CLOEXEC);
    }
    if (fd < 0) {
        return -1;
    }
    atomic_store(&tracemark_fd, fd);
    return 0;
}

bool AtraceIsEnable(void) { return atomic_load(&atrace_enable) && AtraceFd() >= 0; }

void AtraceToggle(bool enable) { atomic_store(&atrace_enable, enable); }

void AtraceCntWrite(const char *tag, long long cnt) { AtraceEmit("C|%d|%s|%lld", self_pid, tag, cnt); }

void AtraceScopeBegin(const char *tag) { AtraceEmit("B|%d|%s", self_pid, tag); }

void AtraceScopeEnd(void) { AtraceEmit("E|%d", self_pid); }
