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

// Empty stubs for the C ABI table `uperf_bridge_t` declared in
// `cpp/include/uperf_rs.h` and implemented in `bridge.cpp`. They exist only so
// the linker can resolve the table structure on the C++ side without forcing
// every binary to link bridge.cpp — `m0_event_tap.cpp` doesn't use them.

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

__attribute__((weak)) int uperf_bridge_subscribe(const char *) { return -1; }
__attribute__((weak)) void uperf_bridge_write_log(const char *, size_t) {}

#ifdef __cplusplus
}
#endif