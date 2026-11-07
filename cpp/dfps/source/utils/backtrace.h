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

#include <sys/cdefs.h>
#include <ucontext.h>

__BEGIN_DECLS

/// Write a raw-address backtrace of `uc` (or of the calling thread when `uc` is null) to
/// stderr. Async-signal-safe: no allocation, no logging, no demangling. `uc` only has to
/// stay valid for the duration of the call.
void DumpBacktrace(const ucontext_t *uc);

/// Install the handler for SIGILL/SIGABRT/SIGFPE/SIGSEGV/SIGBUS/SIGSYS, with an alternate
/// signal stack so a stack-overflow SIGSEGV can still be reported. The handler re-raises the
/// signal with the default disposition, so the process still dies from the original signal.
void SetDumpBacktraceAsCrashHandler(void);

__END_DECLS
