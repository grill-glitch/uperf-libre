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

#pragma once

// Single source of truth for the shared C ABI. `uperf_rs.h` (payload layouts + platform
// helpers) includes this file rather than redeclaring the same entry points: two
// hand-written copies of one prototype list had already diverged, which is undefined
// behaviour across the FFI boundary.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Table passed to the Rust engine at boot. Must stay byte-identical to `ffi::Bridge`
// (`#[repr(C)]` in rust/uperf-core/src/ffi.rs): adding/reordering a field silently
// corrupts the vtable on the Rust side, so the size is pinned at compile time.
typedef struct {
    int  (*subscribe)(const char *topic);
    void (*write_log)(const char *tag, const char *msg, size_t len);
} uperf_bridge_t;

#ifdef __cplusplus
static_assert(sizeof(uperf_bridge_t) == 2 * sizeof(void *), "uperf_bridge_t layout drift vs ffi::Bridge");
#endif

// Functions implemented in Rust (uperf-core), called from C++.
//
// Thread-safety / lifetime contract:
//   * uperf_rs_init must run before any other entry point; `bridge` must be non-null and
//     point to a process-lifetime table (the C++ side passes a `static const` one) and the
//     call is one-shot (a second call is ignored, with a log line).
//   * uperf_rs_start/reload/stop are serialized on the app main thread; start must complete
//     before events arrive.
//   * uperf_rs_on_event is safe to call from any publisher thread. `topic` must be a
//     non-null NUL-terminated string; `data` is borrowed only for the call and may be null
//     iff `len == 0`.
void uperf_rs_init(const uperf_bridge_t *bridge);
int  uperf_rs_start(const char *config_path, const char *log_path);
void uperf_rs_reload(void);
void uperf_rs_stop(void);
void uperf_rs_on_event(const char *topic, const void *data, size_t len);

// Functions implemented in C++ (bridge.cpp), called from Rust or app_main.
// `tag` may be null/empty; `msg` is exactly `len` bytes (no terminator required) and both
// pointers are borrowed for the call only.
const uperf_bridge_t *uperf_bridge_handle(void);
void                  uperf_bridge_write_log(const char *tag, const char *msg, size_t len);
// Returns 0 for a known topic (installing the publishers once), -1 for an unknown one.
int                   uperf_bridge_subscribe(const char *topic);
void                  uperf_bridge_init_rust(const char *config_path, const char *log_path);
// `modules.log.level`; called from Rust once the config is parsed.
void                  uperf_bridge_set_log_level(const char *level);
// `modules.input.{swipeThd,gestureThdX,gestureThdY}`. The listener is built in
// StartPlatform() before the config is parsed, so app_main registers it here and
// Rust applies the values afterwards. Registration must happen before uperf_rs_start on
// the thread that owns the listener; `listener` must stay valid for the process lifetime
// and must not be null (null unregisters).
void                  uperf_register_input_listener(void *listener);
void                  uperf_bridge_set_input_thresholds(float swipeThd, float gestureThdX,
                                                        float gestureThdY);
// `modules.atrace.enable`; opens the ftrace trace_marker and toggles the markers
// emitted by the vendored ATRACE_* instrumentation.
void                  uperf_bridge_set_atrace(bool enable);

#ifdef __cplusplus
}
#endif