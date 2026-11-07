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

#include "inotify.h"
#include "atrace.h"
#include "fmt_exception.h"
#include "misc.h"
#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <poll.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <unistd.h>

constexpr int WAIT_MOVE_US = 500 * 1000;
// Give up on a path that never comes back, so `pendingReadd_` cannot grow without bound.
constexpr int MAX_READD_ATTEMPTS = 20;
// Octal: the old `666` was decimal 666 (an odd, unintended mode).
constexpr int RECREATE_DEFAULT_PERM = 0666;
constexpr std::size_t EVT_BUF_SIZE = 65536;

Inotify::Inotify() : buf_(EVT_BUF_SIZE) {
    // CLOEXEC: the descriptor must not leak into every child this daemon spawns.
    // NONBLOCK so the watcher loop can honour a stop request instead of parking in read().
    fd_ = inotify_init1(IN_CLOEXEC | IN_NONBLOCK);
    if (fd_ < 0) {
        throw FmtException("Cannot make inotify handle");
    }
}

Inotify::~Inotify() {
    for (const auto &[wd, item] : items_) {
        inotify_rm_watch(fd_, wd);
    }
    close(fd_);
}

void Inotify::Add(const std::string &path, int mask, Notifier notifier) {
    mask |= DELETE_SELF | MOVE_SELF;
    auto wd = inotify_add_watch(fd_, path.c_str(), mask);
    if (wd < 0) {
        throw FmtException("Cannot watch file '{}'", path);
    }

    WatchItem item;
    item.path = path;
    item.mask = mask;
    item.notifier = std::move(notifier);
    // The watcher thread may be inserting/erasing in the same map right now.
    std::lock_guard<std::mutex> lock(mutex_);
    items_[wd] = std::move(item);
}

bool Inotify::WaitForEvent(int timeoutMs) {
    struct pollfd pfd;
    pfd.fd = fd_;
    pfd.events = POLLIN;
    pfd.revents = 0;
    for (;;) {
        auto ret = poll(&pfd, 1, timeoutMs);
        if (ret > 0) {
            return (pfd.revents & POLLIN) != 0;
        }
        if (ret == 0) {
            // Nothing to read: the only other work this thread has, so retries happen here
            // instead of inside the dispatch loop.
            ProcessPendingReadd();
            return false;
        }
        if (errno != EINTR) {
            throw FmtException("Cannot poll inotify events: {}", strerror(errno));
        }
    }
}

bool Inotify::Remove(const std::string &path) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto it = items_.begin(); it != items_.end(); ++it) {
        if (it->second.path != path) {
            continue;
        }
        inotify_rm_watch(fd_, it->first);
        items_.erase(it);
        return true;
    }
    return false;
}

void Inotify::WaitAndHandle(void) {
    ATRACE_SCOPE(InotifyHandle);
    auto len = read(fd_, buf_.data(), buf_.size());
    if (len < 0) {
        // EAGAIN: the descriptor is non-blocking and the queue is empty (spurious wakeup
        // from poll). Not an error.
        if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) {
            return;
        }
        throw FmtException("Cannot read inotify events: {}", strerror(errno));
    }
    ssize_t off = 0;
    while (off < len) {
        // Validate the whole record, not just the header: a truncated tail must not make
        // `off` overshoot and silently drop the events that followed it.
        if (off + static_cast<ssize_t>(sizeof(struct inotify_event)) > len) {
            break;
        }
        auto evt = reinterpret_cast<struct inotify_event *>(buf_.data() + off);
        auto recLen = static_cast<ssize_t>(sizeof(struct inotify_event) + evt->len);
        if (off + recLen > len) {
            break;
        }
        off += recLen;
        // Copy under the lock, then release it before invoking the notifier (which may
        // call back into `Add()`), so no lock is ever held across user code.
        WatchItem item;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            auto it = items_.find(evt->wd);
            if (it == items_.end()) {
                continue;
            }
            item = it->second;
        }
        if (item.notifier && (evt->mask & item.mask)) {
            item.notifier(evt->len ? evt->name : item.path, evt->mask);
        }
        // Re-establish watching after deleting. The old code slept 500 ms right here to
        // wait for an in-place rewrite; that blocked delivery of every other watch's events
        // (and could overflow the kernel queue). The retry is queued instead, so this batch
        // is dispatched immediately and `ProcessPendingReadd()` picks the watch up on a
        // later loop turn.
        if (evt->mask & (IGNORED | DELETE_SELF | MOVE_SELF)) {
            bool readded = false;
            {
                std::lock_guard<std::mutex> lock(mutex_);
                auto newWd = inotify_add_watch(fd_, item.path.c_str(), item.mask);
                if (newWd >= 0) {
                    if (newWd != evt->wd) {
                        items_[newWd] = item;
                        items_.erase(evt->wd);
                    }
                    readded = true;
                }
            }
            if (!readded) {
                // Re-adding failed (the file is gone at the moment): retry later, and never
                // insert under the -1 descriptor.
                std::lock_guard<std::mutex> lock(mutex_);
                items_.erase(evt->wd);
                // The notifier travels with the pending entry: erasing the watch entry
                // destroys the only copy of the callback.
                pendingReadd_.push_back(PendingReadd{item, GetNowTs() + WAIT_MOVE_US, 1});
            }
        }
    }
}

void Inotify::ProcessPendingReadd(void) {
    if (pendingReadd_.empty()) {
        return;
    }
    const auto now = GetNowTs();
    std::vector<PendingReadd> stillPending;
    stillPending.reserve(pendingReadd_.size());
    for (auto &p : pendingReadd_) {
        if (p.nextTryUs > now) {
            stillPending.push_back(std::move(p));
            continue;
        }
        // The file is usually back by now (a rename into place). Recreate it only when it is
        // still missing: `creat` is what the old in-loop code did, just without the sleep.
        if (access(p.item.path.c_str(), F_OK) != 0) {
            // creat() returning 0 is a valid descriptor; only < 0 is a failure.
            int recreated = creat(p.item.path.c_str(), RECREATE_DEFAULT_PERM);
            if (recreated >= 0) {
                close(recreated);
            }
        }
        bool ok = false;
        {
            std::lock_guard<std::mutex> lock(mutex_);
            auto wd = inotify_add_watch(fd_, p.item.path.c_str(), p.item.mask);
            if (wd >= 0) {
                items_[wd] = p.item;
                ok = true;
            }
        }
        if (!ok) {
            p.attempts += 1;
            p.nextTryUs = now + WAIT_MOVE_US;
            if (p.attempts <= MAX_READD_ATTEMPTS) {
                stillPending.push_back(std::move(p));
            }
        }
    }
    pendingReadd_ = std::move(stillPending);
}
