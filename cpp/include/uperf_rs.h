/*
 * Copyright (C) 2026 grill-glitch
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

/*
 * The C ABI between the reused C++ platform and the Rust policy engine.
 *
 * The declarations live in `uperf_rs_bridge.h` (included below) — this file adds the
 * payload layouts and the compile-time layout checks, so there is one copy of each
 * prototype. Rust exports every symbol declared there; the payload structs are pinned by
 * static_assert because the Rust decoder reads fixed offsets.
 *
 * Payload rule (AGENT.md §5.1): Rust never dereferences C++ objects. Every topic is
 * converted in cpp/uperf/bridge.cpp into a C-layout payload before crossing this
 * boundary, and `data` is only valid for the duration of uperf_rs_on_event().
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

// The entry points themselves live in one place.
#include "uperf_rs_bridge.h"

#ifdef __cplusplus
extern "C" {
#endif

/* --- Payload layouts (built by bridge.cpp, consumed by Rust) ------------------------*/

/* input.touch / input.btn / offscreen.state: int32_t 0|1 */

typedef struct {
    int32_t in_hold;
    int32_t in_swipe;
    int32_t in_gesture;
} uperf_input_state_t; /* input.state */

/* cgroup.*.list */
typedef struct {
    const int32_t *pids;
    size_t len;
} uperf_pid_list_t;

/* topapp.pkgName: NUL-terminated UTF-8 string.

   The former `uperf_bridge_dw_*` / `uperf_bridge_hw_*` / `uperf_bridge_sched_*` /
   `uperf_bridge_screen_brightness` / `uperf_bridge_os_version` /
   `uperf_bridge_set_thread_name` declarations were removed: nothing implemented or called
   them (the scheduler and the device probes live in Rust now), so they were prototypes for
   symbols that did not exist — any caller would have failed to link. */

#ifdef __cplusplus
} // extern "C"
#endif

#if defined(__cplusplus) && defined(__aarch64__)
/* The shipped module is arm64-only, and the Rust decoder reads `size_t` at the target
   width: a 32-bit build would misread `uperf_pid_list_t` as 8 bytes. */
static_assert(sizeof(void *) == 8, "uperf_pid_list_t is LP64-only (see topic_dispatch.rs)");
static_assert(sizeof(uperf_pid_list_t) == 16, "uperf_pid_list_t layout drift");
static_assert(sizeof(uperf_input_state_t) == 12, "uperf_input_state_t layout drift");
#endif
