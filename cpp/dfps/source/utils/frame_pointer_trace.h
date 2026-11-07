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

// __BEGIN_DECLS lives in <sys/cdefs.h>; size_t comes from <stddef.h> (not <stdlib.h>, which
// this header does not otherwise need).
#include <stddef.h>
#include <stdint.h>
#include <sys/cdefs.h>

__BEGIN_DECLS

/// Walk the frame-pointer chain of the calling thread.
///
/// Preconditions (all checked): `out_trace` non-null, `max_depth` > 0, and the code built
/// with -fno-omit-frame-pointer. `skip_initial` counts raw frames including this function
/// and its caller, so pass >= 1 to skip the walker itself.
///
/// Returns the number of frames written — a short result means the chain ended, the buffer
/// was full, or the stack bounds could not be determined, and the caller cannot tell those
/// apart. Frames are raw return addresses: symbolise out of process.
size_t trace_stackframepointers(void **out_trace, size_t max_depth, size_t skip_initial);

__END_DECLS
