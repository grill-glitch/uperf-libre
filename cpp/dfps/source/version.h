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

// __BEGIN_DECLS comes from <sys/cdefs.h>; <stdlib.h> only pulls it in transitively on
// glibc/bionic, and this header needs no stdlib declaration of its own.
#include <sys/cdefs.h>

__BEGIN_DECLS

/// The build's git commit hash, as a NUL-terminated string.
/// The pointer refers to static storage owned by the library: do not free() it, it stays
/// valid for the process lifetime and it is never NULL. Thread-safe (immutable).
const char *GetGitCommitHash(void);

__END_DECLS
