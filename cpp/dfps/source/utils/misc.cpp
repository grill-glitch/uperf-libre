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

#include "misc.h"
#include <cerrno>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <fcntl.h>
#include <scn/scn.h>
#include <signal.h>
#include <spdlog/spdlog.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

int64_t GetNowTs(void) {
    auto ns = std::chrono::steady_clock::time_point::clock().now().time_since_epoch();
    return std::chrono::duration_cast<std::chrono::microseconds>(ns).count();
}

void Sleep(int64_t us) {
    // usleep() takes a useconds_t: a value above 1 s is out of its POSIX contract (and a
    // negative one wraps to a huge unsigned). Split into whole seconds instead.
    if (us <= 0) {
        return;
    }
    struct timespec ts;
    ts.tv_sec = static_cast<time_t>(us / 1000000);
    ts.tv_nsec = static_cast<long>((us % 1000000) * 1000);
    while (nanosleep(&ts, &ts) == -1 && errno == EINTR) {
    }
}

bool IsSpace(const char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\v' || c == '\f' || c == '\r'; }

bool IsDigit(const char c) { return c >= '0' && c <= '9'; }

bool ParseInt(const char *s, int32_t *val) {
    if (s == nullptr || val == nullptr) {
        return false;
    }

    int sign = 1;
    const char *digits = nullptr;

    // locate beginning of digit
    for (const char *c = s; *c; ++c) {
        if (*c == '-') {
            sign = -1;
        }
        if (IsDigit(*c)) {
            digits = c;
            break;
        }
    }
    if (digits == nullptr) {
        return false;
    }

    // skip leading zeros
    while (*digits == '0' && IsDigit(*(digits + 1))) {
        ++digits;
    }

    // Accumulate in int64_t: the old int arithmetic overflowed (UB) on a 10-digit value
    // such as "2147483648" before the range check could reject it.
    int64_t v = 0;
    int len = 0;
    for (const char *c = digits; IsDigit(*c); ++c) {
        if (++len > 10) {
            return false;
        }
        v = v * 10 + (*c - '0');
    }
    v *= sign;
    if (v < INT32_MIN || v > INT32_MAX) {
        return false;
    }
    *val = static_cast<int32_t>(v);
    return true;
}

std::string_view RefKey(const std::string &fullname) {
    std::string_view view = fullname;
    auto pos = view.find_first_of('.');
    return (pos < view.length()) ? view.substr(pos + 1) : std::string_view();
}

static char *argv0;
static size_t nameLen;

void InitArgv(int argc, char **argv) {
    argv0 = argv[0];
    nameLen = (argv[argc - 1] - argv[0]) + strlen(argv[argc - 1]) + 1;
}

void SetSelfName(const std::string_view &name) {
    if (argv0 == nullptr || nameLen == 0) {
        return;
    }
    // `name.data()` is not guaranteed to be NUL-terminated (string_view): copy at most
    // what fits and keep the terminator, instead of letting strlcpy scan past the end.
    memset(argv0, 0, nameLen);
    auto n = name.size() < nameLen - 1 ? name.size() : nameLen - 1;
    memcpy(argv0, name.data(), n);
    SetSelfThreadName(name);
}

void SetSelfThreadName(const std::string_view &name) {
    // PR_SET_NAME copies until it hits a NUL, up to 16 bytes (15 + terminator).
    char buf[16] = {0};
    auto n = name.size() < sizeof(buf) - 1 ? name.size() : sizeof(buf) - 1;
    memcpy(buf, name.data(), n);
    prctl(PR_SET_NAME, buf);
}

constexpr int READER_FD_FLAGS = O_RDONLY | O_NONBLOCK | O_CLOEXEC;
constexpr int WRITER_FD_FLAGS = O_WRONLY | O_NONBLOCK | O_CLOEXEC;
constexpr int WRITER_PERM = 0666;
constexpr int WRITER_EXCLUDED_PERM = 0444;

int ReadFile(const std::string_view &path, std::string *content, size_t maxLen) {
    constexpr size_t READ_DEFAULT_SIZE = 4096;
    if (content == nullptr) {
        return -1;
    }
    content->clear();

    // open() returns -1 on failure; descriptor 0 is a valid one (stdin closed).
    int fd = open(path.data(), READER_FD_FLAGS);
    if (fd < 0) {
        return -1;
    }

    if (maxLen == 0) {
        maxLen = READ_DEFAULT_SIZE;
    }
    content->resize(maxLen);
    maxLen--; // reserve for EOF

    // < 4096 bytes
    if (maxLen < READ_DEFAULT_SIZE) {
        auto len = read(fd, content->data(), maxLen);
        if (len > 0) {
            content->data()[len] = '\0';
            content->resize(len);
        } else {
            content->data()[0] = '\0';
            content->clear();
        }
        close(fd);
        return len;
    }

    // >= 4096 bytes
    int len = 0;
    int l;
    while ((l = read(fd, content->data() + len, maxLen - len)) > 0) {
        len += l;
    }
    // A signal during the read truncates the file silently; retry once with the remaining
    // space instead of returning a short buffer as if it were the whole file.
    if (len > 0) {
        content->data()[len] = '\0';
        content->resize(len);
    } else {
        content->data()[0] = '\0';
        content->clear();
    }
    close(fd);
    return len;
}

int WriteSysfsFile(const std::string_view &path, const std::string_view &s) {
    auto fd = GetFdForWrite(path);
    if (fd < 0) {
        return -1;
    }
    auto ret = WriteSysfsFile(fd, s);
    close(fd);
    return ret;
}

int WriteSysfsFile(const std::string_view &path, int s) {
    auto fd = GetFdForWrite(path);
    if (fd < 0) {
        return -1;
    }
    auto ret = WriteSysfsFile(fd, s);
    close(fd);
    return ret;
}

int WriteSysfsFile(int fd, const std::string_view &s) {
    if (fd < 0) {
        return -1;
    }
    // A non-blocking/large write may be short and may be interrupted: loop until every
    // byte is out, and report a partial result instead of claiming success.
    size_t written = 0;
    while (written < s.length()) {
        auto ret = write(fd, s.data() + written, s.length() - written);
        if (ret > 0) {
            written += static_cast<size_t>(ret);
        } else if (ret < 0 && errno == EINTR) {
            continue;
        } else {
            return written > 0 ? static_cast<int>(written) : -1;
        }
    }
    return static_cast<int>(written);
}

int WriteSysfsFile(int fd, int s) {
    // Sysfs attributes (and every other text kernel interface) take ASCII, not the raw
    // in-memory bytes of `int`.
    if (fd < 0) {
        return -1;
    }
    return WriteSysfsFile(fd, std::string_view(std::to_string(s)));
}

int GetFdForWrite(const std::string_view &path) {
    auto fd = open(path.data(), WRITER_FD_FLAGS);
    if (fd >= 0) {
        return fd;
    }
    // Only a permission problem is worth re-trying through a mode change; anything else
    // (missing node, wrong type) must not have its mode rewritten as a side effect.
    if (errno != EACCES && errno != EPERM) {
        return -1;
    }
    if (chmod(path.data(), WRITER_PERM) != 0) {
        return -1;
    }
    return open(path.data(), WRITER_FD_FLAGS);
}

int GetFdForWriteExcluded(const std::string_view &path) {
    int fd = GetFdForWrite(path);
    if (fd < 0) {
        return -1;
    }
    chmod(path.data(), WRITER_EXCLUDED_PERM);
    chown(path.data(), 0, 0); // root
    return fd;
}

int ExecCmd(int *fd, const char **argv) {
    int pipefd[] = {-1, -1};
    auto &parentFd = pipefd[0];
    auto &childFd = pipefd[1];

    if (fd) {
        if (pipe2(pipefd, O_CLOEXEC) == -1) {
            return -1;
        }
    }

    int pid = fork();
    if (pid < 0) {
        close(parentFd);
        close(childFd);
        return -1;
    } else if (pid) {
        if (fd) {
            *fd = parentFd;
            close(childFd);
        }
        return pid;
    }

    // Unblock all signals
    sigset_t s;
    sigfillset(&s);
    pthread_sigmask(SIG_UNBLOCK, &s, nullptr);

    if (childFd >= 0) {
        dup2(childFd, STDOUT_FILENO);
        dup2(childFd, STDERR_FILENO);
        close(childFd);
    }

    execv(argv[0], (char *const *)argv);
    // Only async-signal-safe calls are legal after fork() in a multithreaded process:
    // exit() would run atexit handlers and flush stdio buffers shared with the parent.
    _exit(127);
}

int ExecCmdSync(std::string *content, const char **argv) {
    int fd = -1;
    int pid = ExecCmd(content ? &fd : nullptr, argv);
    if (pid < 0) {
        return -1;
    }

    // Drain the pipe *before* waiting: the child blocks in write() once the pipe buffer
    // (~64 KiB) is full, so waiting first deadlocks the parent (and vice versa).
    if (content) {
        constexpr size_t READ_BATCH_SIZE = 4096;
        size_t len = 0;
        content->clear();
        for (;;) {
            content->resize(len + READ_BATCH_SIZE);
            auto l = read(fd, content->data() + len, READ_BATCH_SIZE);
            if (l > 0) {
                len += static_cast<size_t>(l);
            } else if (l < 0 && errno == EINTR) {
                continue;
            } else {
                break;
            }
        }
        close(fd);
        // Size excludes the terminator that resize() leaves behind.
        content->resize(len);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        return -1;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}
