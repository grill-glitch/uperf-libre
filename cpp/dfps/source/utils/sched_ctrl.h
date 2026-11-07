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

#include <sched.h>
#include <stdbool.h>
#include <stdlib.h>

__BEGIN_DECLS

// tid: kernel thread id. mask: non-null, CPU_SETSIZE-based (a dynamically sized
// CPU_ALLOC set cannot be expressed here), valid for the call. Returns 0 or -1 with errno.
int SchedCtrlSetAffinity(int tid, const cpu_set_t *mask);

// Logical priority of `tid` in the same encoding SchedCtrlSetStaticPrio() accepts:
// 1..98 = SCHED_FIFO/RR (kernel 98..1), 100..139 = SCHED_NORMAL (nice -20..19),
// 140 = SCHED_BATCH, 141 = SCHED_IDLE. Returns -1 for SCHED_DEADLINE (no static priority)
// and for any failure.
int SchedCtrlGetStaticPrio(int tid);

// prio: logical 1..98 => SCHED_FIFO (kernel priority 99-prio, so never 0), 100..139 =>
// SCHED_NORMAL with nice = prio-120. reset_on_fork applies SCHED_RESET_ON_FORK so a child
// does not inherit the privileged policy. Returns 0, or -1 with errno (EINVAL for an
// out-of-range prio).
int SchedCtrlSetStaticPrio(int tid, int prio, bool reset_on_fork);

// policy/prio go straight to sched_setscheduler(): prio must be 0 for SCHED_NORMAL /
// SCHED_BATCH / SCHED_IDLE and 1..99 for SCHED_FIFO / SCHED_RR. Raw return value.
int SchedCtrlSetSchedClass(int tid, int policy, int prio);

// Permanently removes CAP_SYS_NICE from tid's effective/permitted/inheritable sets.
// tid == 0 means the calling thread. Requires CAP_SETPCAP. Irreversible for the target:
// do not pass the main thread id unless losing scheduling privileges is intended.
// Returns 0, or -1 with errno (EPERM/ESRCH/EFAULT).
int SchedCtrlRemoveNiceCap(int tid);

__END_DECLS
