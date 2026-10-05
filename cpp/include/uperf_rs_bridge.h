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

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Table passed to the Rust engine at boot. Mirror of `ffi::Bridge` in
// `rust/uperf-core/src/ffi.rs` — keep the two in sync.
typedef struct {
    int  (*subscribe)(const char *topic);
    void (*write_log)(const char *tag, const char *msg, size_t len);
} uperf_bridge_t;

// Functions implemented in Rust (uperf-core), called from C++.
void uperf_rs_init(const uperf_bridge_t *bridge);
int  uperf_rs_start(const char *config_path, const char *log_path);
void uperf_rs_reload(void);
void uperf_rs_stop(void);
void uperf_rs_on_event(const char *topic, const void *data, size_t len);

// Functions implemented in C++ (bridge.cpp), called from Rust or app_main.
const uperf_bridge_t *uperf_bridge_handle(void);
void                  uperf_bridge_write_log(const char *tag, const char *msg, size_t len);
int                   uperf_bridge_subscribe(const char *topic);
void                  uperf_bridge_init_rust(const char *config_path, const char *log_path);
// `modules.log.level`; called from Rust once the config is parsed.
void                  uperf_bridge_set_log_level(const char *level);
// `modules.input.{swipeThd,gestureThdX,gestureThdY}`. The listener is built in
// StartPlatform() before the config is parsed, so app_main registers it here and
// Rust applies the values afterwards.
void                  uperf_register_input_listener(void *listener);
void                  uperf_bridge_set_input_thresholds(float swipeThd, float gestureThdX,
                                                        float gestureThdY);

#ifdef __cplusplus
}
#endif