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

#include <cstdint>
#include <functional>
#include <mutex>
#include <string>
#include <unordered_map>
#include <vector>

class Inotify {
public:
    using Notifier = std::function<void(const std::string &, int)>;
    enum {
        ACCESS = 0x00000001,
        MODIFY = 0x00000002,
        ATTRIB = 0x00000004,
        CLOSE_WRITE = 0x00000008,
        CLOSE_NOWRITE = 0x00000010,
        OPEN = 0x00000020,
        MOVED_FROM = 0x00000040,
        MOVED_TO = 0x00000080,
        CREATE = 0x00000100,
        DELETE = 0x00000200,
        DELETE_SELF = 0x00000400,
        MOVE_SELF = 0x00000800,
        UNMOUNT = 0x00002000,
        Q_OVERFLOW = 0x00004000,
        IGNORED = 0x00008000,
        CLOSE = (CLOSE_WRITE | CLOSE_NOWRITE),
        MOVE = (MOVED_FROM | MOVED_TO),
        ONLYDIR = 0x01000000,
        DONT_FOLLOW = 0x02000000,
        EXCL_UNLINK = 0x04000000,
        MASK_CREATE = 0x10000000,
        MASK_ADD = 0x20000000,
        ISDIR = 0x40000000,
        ONESHOT = 0x80000000
    };

    Inotify();
    ~Inotify();

    // Thread-safe: `Add` may be called from any thread while the watcher thread is inside
    // WaitAndHandle(). Copy/move are deleted because both would duplicate `fd_` (double
    // close) and the watch descriptors.
    Inotify(const Inotify &) = delete;
    Inotify &operator=(const Inotify &) = delete;
    Inotify(Inotify &&) = delete;
    Inotify &operator=(Inotify &&) = delete;

    void Add(const std::string &path, int mask, Notifier notifier);
    /// Stop watching `path` (and forget its callback). Returns false when it was unwatched.
    bool Remove(const std::string &path);
    /// Wait up to `timeoutMs` for an event. `false` = timeout (no event pending).
    bool WaitForEvent(int timeoutMs);
    /// Read and dispatch one batch. Safe to call only when WaitForEvent() returned true
    /// (the descriptor is non-blocking, so an empty read is a no-op).
    void WaitAndHandle(void);
    /// The underlying descriptor, so a caller can poll it together with other fds.
    int Fd(void) const { return fd_; }

private:
    struct WatchItem {
        Notifier notifier;
        std::string path;
        int mask;
    };

    /// A watch the kernel dropped (`IN_IGNORED`/`*_SELF`) whose path is not back yet.
    /// Re-adding is retried from the watcher loop instead of sleeping inside the event
    /// dispatch: a 500 ms sleep there held up every other watch and could overflow the
    /// kernel queue.
    struct PendingReadd {
        WatchItem item;
        int64_t nextTryUs;
        int attempts;
    };

    /// Retry the due `pendingReadd_` entries. Watcher thread only (also called on poll
    /// timeout), so no extra locking beyond `mutex_` for the table itself.
    void ProcessPendingReadd(void);

    int fd_;
    std::unordered_map<int, WatchItem> items_;
    std::vector<PendingReadd> pendingReadd_;
    std::vector<uint8_t> buf_;
    // Serialises the watch table: `Add()` inserts while the watcher thread looks up and
    // erases. Never held while a notifier runs (a notifier may call `Add`).
    std::mutex mutex_;
};
