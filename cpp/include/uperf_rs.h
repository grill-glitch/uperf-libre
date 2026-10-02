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
 * Implementation status:
 *   M0: declared only, nothing calls these yet (the M0 binary is pure C++).
 *   M1: implemented by rust/uperf-core (exported symbols) — must stay equal to the
 *       output of `cbindgen --check` for that crate.
 *
 * Payload rule (AGENT.md §5.1): Rust never dereferences C++ objects. Every topic is
 * converted in cpp/uperf/bridge.cpp into a C-layout payload before crossing this
 * boundary, and `data` is only valid for the duration of uperf_rs_on_event().
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* --- Rust exports (called from C++) -------------------------------------------------*/

/* Start the policy engine. config_path/log_path are UTF-8, NUL-terminated and must stay
 * alive for the process lifetime. Returns 0 on success.
 * NOTE: the daemon's `-o` file is opened by the C++ logger sink before this is called;
 * log_path is passed for parity of the config/log pair and for the Rust-side log lines. */
int uperf_rs_start(const char *config_path, const char *log_path);

/* Config file was rewritten (dfps supervisor's SIGUSR1 restart path): reload in place,
 * keep the process alive. */
void uperf_rs_reload(void);

/* Child process is going away; stop workers and release fds. */
void uperf_rs_stop(void);

/* Event with a C-layout payload; `data` may be NULL when the topic carries no payload.
 * `len` is the payload size in bytes (0 when data is NULL). */
void uperf_rs_on_event(const char *topic, const void *data, size_t len);

/* --- Topic payload layouts (built by bridge.cpp, consumed by Rust) ------------------*/

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

/* topapp.pkgName: NUL-terminated UTF-8 string */

/* --- C++ platform, callable from Rust (implemented in cpp/uperf/bridge.cpp) ---------*/

/* Subscribe a topic on the CoBridge; the payload is converted to the layouts above.
 * Returns 0 on success, -1 on unknown topic. */
int uperf_bridge_subscribe(const char *topic);

/* Delayed/Heavy worker handles (DelayedWorker/HeavyWorker singletons). */
uint64_t uperf_bridge_dw_create(const char *name);
void uperf_bridge_dw_set(uint64_t handle, void (*cb)(void *), void *ud, int64_t ts_us);
uint64_t uperf_bridge_hw_create(const char *name);
void uperf_bridge_hw_set(uint64_t handle, void (*cb)(void *), void *ud);

/* utils/sched_ctrl.c */
int uperf_bridge_sched_set_prio(int tid, int prio, int reset_on_fork);
int uperf_bridge_sched_set_affinity(int tid, const uint8_t *cpumask_bytes, size_t n);
int uperf_bridge_sched_set_class(int tid, int policy, int prio);

/* utils/misc_android.cpp */
int uperf_bridge_screen_brightness(void);
int uperf_bridge_os_version(void);

/* utils/misc.cpp */
void uperf_bridge_set_thread_name(const char *name);

#ifdef __cplusplus
}
#endif
