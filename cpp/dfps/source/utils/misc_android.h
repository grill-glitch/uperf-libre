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

#include <string>

/// `ro.build.version.release` parsed as an integer major version (10 for "10", 7 for "7.1").
/// Returns 0 when the property is absent or unparseable.
int GetOSVersion(void);

/// Top app package via `dumpsys`. Blocking (~40-100 ms, forks a child). Empty string when the
/// probe failed or the OS version is below 7 — a failure cannot be told apart from "no top
/// app", so callers must treat empty as "no news".
std::string GetTopAppNameDumpsys(void);

/// Launcher package via `cmd package resolve-activity`. Blocking; empty string on failure.
std::string GetHomePackageName(void);

/// Blocking: scans the privileged /data/tombstones directory and reads the newest matching
/// tombstone (capped at 512 KiB). `pid` must be > 0. Returns an empty string both when no
/// tombstone matches the pid and on any read error (missing permission, unreadable file).
std::string GetTombstone(int pid);

/// Current `settings get system screen_brightness`. Returns -1 when the value could not be
/// read (no output, i.e. SELinux denial, or non-integer output) — -1 is also a plausible
/// "unset brightness", so the sentinel is part of the contract.
int GetScreenBrightness(void);

/// Writes system.peak_refresh_rate / system.min_refresh_rate / system.miui_refresh_rate and
/// secure.miui_refresh_rate. `hz` must be numeric in 1..240 (anything else is ignored, so a
/// config typo cannot be written into the settings provider). Fire-and-forget: the `cmd
/// settings put` children run asynchronously and their status is not observed.
void SysPeakRefreshRate(const std::string &hz);

/// Sets the SurfaceFlinger active config via a raw binder transaction (>= Android 10).
/// Requires binder access from this process's SELinux domain (root/shell only); the status
/// of `service call` is not checked, so a permission denial is silent.
/// `force` (>= Android 11) also does the frame-rate flexibility token dance.
void SysSurfaceflingerBackdoor(int configIndex, bool force);
