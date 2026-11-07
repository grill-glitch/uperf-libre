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

// Crash handler.
//
// Everything reachable from here runs in signal context, where only async-signal-safe
// functions are legal. The previous implementation called spdlog (mutex + malloc + file
// I/O), std::string, __cxa_demangle (malloc) and exit() (atexit + stdio flush) from the
// handler — a crash inside the allocator or while the log mutex was held deadlocked the
// process instead of reporting it. This version formats into a static buffer with hand
// rolled integer conversion and emits it with write(2); frames are raw addresses, which are
// symbolised offline (the tombstone path still gives symbolised traces on Android).

#include "backtrace.h"
#include "frame_pointer_trace.h"
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <unwind.h>

namespace {

constexpr size_t MAX_FRAMES = 64;
constexpr size_t CRASH_BUF_SIZE = 4096;
// Leave room for one frame line even if a write fails.
constexpr size_t CRASH_BUF_LOW_WATER = CRASH_BUF_SIZE - 128;

char g_buf[CRASH_BUF_SIZE];
size_t g_len = 0;
volatile sig_atomic_t g_signal = 0;

void Flush(void) {
    size_t off = 0;
    while (off < g_len) {
        auto n = write(STDERR_FILENO, g_buf + off, g_len - off);
        if (n <= 0) {
            break;
        }
        off += static_cast<size_t>(n);
    }
    g_len = 0;
}

void AppendChar(char c) {
    if (g_len < CRASH_BUF_SIZE) {
        g_buf[g_len++] = c;
    }
}

void AppendStr(const char *s) {
    while (*s != '\0') {
        AppendChar(*s++);
    }
}

void AppendHex(uintptr_t v) {
    char tmp[2 * sizeof(uintptr_t)];
    int n = 0;
    do {
        int digit = static_cast<int>(v & 0xf);
        tmp[n++] = static_cast<char>(digit < 10 ? '0' + digit : 'a' + digit - 10);
        v >>= 4;
    } while (v != 0);
    while (n > 0) {
        AppendChar(tmp[--n]);
    }
}

void AppendDec(unsigned int v) {
    char tmp[10];
    int n = 0;
    do {
        tmp[n++] = static_cast<char>('0' + v % 10);
        v /= 10;
    } while (v != 0);
    while (n > 0) {
        AppendChar(tmp[--n]);
    }
}

struct UnwindState {
    size_t frame;
    // Skip everything until the interrupted PC/LR is reached: the frames above it are the
    // signal trampoline and this handler, which are noise for the reader.
    uintptr_t sig_pc;
    uintptr_t sig_lr;
    bool found;
    uintptr_t prev_pc;
    uintptr_t prev_sp;
};

_Unwind_Reason_Code UnwindCallback(struct _Unwind_Context *ctx, void *arg) {
    auto *self = static_cast<UnwindState *>(arg);
    auto pc = static_cast<uintptr_t>(_Unwind_GetIP(ctx));
    auto sp = static_cast<uintptr_t>(_Unwind_GetCFA(ctx));

    if (!self->found) {
        // pc may be the interrupted PC itself or one instruction past it.
        if ((self->sig_pc != 0 && pc >= self->sig_pc - 4 && pc <= self->sig_pc + 4) ||
            (self->sig_lr != 0 && pc >= self->sig_lr - 4 && pc <= self->sig_lr + 4)) {
            self->found = true;
        } else {
            return _URC_NO_REASON;
        }
    }

    if (self->frame > 0 && pc == self->prev_pc && sp == self->prev_sp) {
        return _URC_END_OF_STACK;
    }

    AppendStr("  #");
    if (self->frame < 10) {
        AppendChar('0');
    }
    AppendDec(static_cast<unsigned int>(self->frame));
    AppendStr(" 0x");
    AppendHex(pc);
    AppendChar('\n');
    if (g_len >= CRASH_BUF_LOW_WATER) {
        Flush();
    }

    self->prev_pc = pc;
    self->prev_sp = sp;
    if (++self->frame >= MAX_FRAMES) {
        return _URC_END_OF_STACK;
    }
    return _URC_NO_REASON;
}

} // namespace

void DumpBacktrace(const ucontext_t *uc) {
    UnwindState self{};
#if defined(__aarch64__)
    if (uc != nullptr) {
        self.sig_pc = static_cast<uintptr_t>(uc->uc_mcontext.pc);
        self.sig_lr = static_cast<uintptr_t>(uc->uc_mcontext.regs[30]);
    }
#elif defined(__arm__)
    if (uc != nullptr) {
        self.sig_pc = static_cast<uintptr_t>(uc->uc_mcontext.arm_pc);
        self.sig_lr = static_cast<uintptr_t>(uc->uc_mcontext.arm_lr);
    }
#endif
    AppendStr("Unwind backtrace (raw addresses):\n");
    _Unwind_Backtrace(UnwindCallback, &self);
    // A frame-pointer walk is the fallback when the unwinder produced nothing usable; it
    // never allocates, so it is safe here too.
    if (self.frame == 0) {
        void *pointers[MAX_FRAMES];
        auto len = trace_stackframepointers(pointers, MAX_FRAMES, 1);
        AppendStr("Framepointer backtrace (raw addresses):\n");
        for (size_t i = 0; i < len; ++i) {
            AppendStr("  #");
            if (i < 10) {
                AppendChar('0');
            }
            AppendDec(static_cast<unsigned int>(i));
            AppendStr(" 0x");
            AppendHex(reinterpret_cast<uintptr_t>(pointers[i]));
            AppendChar('\n');
            if (g_len >= CRASH_BUF_LOW_WATER) {
                Flush();
            }
        }
    }
}

namespace {

void CrashSignalHandler(int sig, siginfo_t *si, void *uc) {
    g_signal = sig;
    g_len = 0;
    AppendStr("\n*** uperf: fatal signal ");
    AppendDec(static_cast<unsigned int>(sig));
    AppendStr(" ***\n");
    if (si != nullptr) {
        AppendStr("fault addr 0x");
        AppendHex(reinterpret_cast<uintptr_t>(si->si_addr));
        AppendChar('\n');
    }
    Flush();
    DumpBacktrace(static_cast<const ucontext_t *>(uc));
    Flush();
    AppendStr("*** end of backtrace (symbolise offline) ***\n");
    Flush();

    // Re-raise with the default disposition so the exit status and core-dump behaviour are
    // the ones a crash should produce (exit() would have run atexit handlers and flushed
    // stdio, neither of which is async-signal-safe).
    signal(sig, SIG_DFL);
    raise(sig);
    _exit(EXIT_FAILURE);
}

} // namespace

void SetDumpBacktraceAsCrashHandler(void) {
    static const int crashSignals[] = {SIGILL, SIGABRT, SIGFPE, SIGSEGV, SIGBUS, SIGSYS};
    // A stack-overflow SIGSEGV cannot run a handler on the exhausted stack: without an
    // alternate stack + SA_ONSTACK the backtrace was simply lost.
    // Fixed size: SIGSTKSZ is a sysconf() call (i.e. a VLA) on some libcs.
    constexpr size_t ALT_STACK_SIZE = 64 * 1024;
    static char altStack[ALT_STACK_SIZE];
    stack_t ss;
    memset(&ss, 0, sizeof(ss));
    ss.ss_sp = altStack;
    ss.ss_size = ALT_STACK_SIZE;
    ss.ss_flags = 0;
    bool onStack = sigaltstack(&ss, nullptr) == 0;

    struct sigaction act;
    memset(&act, 0, sizeof(act));
    sigfillset(&act.sa_mask);
    act.sa_sigaction = CrashSignalHandler;
    act.sa_flags = SA_RESTART | SA_SIGINFO | (onStack ? SA_ONSTACK : 0);

    for (int sig : crashSignals) {
        if (sigaction(sig, &act, nullptr) != 0) {
            // Not in signal context: the normal logger is fine here.
            const char msg[] = "uperf: cannot install the crash handler\n";
            (void)!write(STDERR_FILENO, msg, sizeof(msg) - 1);
        }
    }
}
