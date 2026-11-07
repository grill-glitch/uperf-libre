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

#include "misc_android.h"
#include "utils/misc.h"
#include <atomic>
#include <cstring>
#include <dirent.h>
#include <stdlib.h>
#include <string>
#include <sys/stat.h>
#include <sys/system_properties.h>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

// -1 = "not read yet": 0 is a legitimate value and used to be indistinguishable from the
// uninitialised cache.
static std::atomic<int> androidOsVersion{-1};

int GetOSVersion(void) {
    int cached = androidOsVersion.load(std::memory_order_relaxed);
    if (cached >= 0) {
        return cached;
    }

    char version[PROP_VALUE_MAX + 1] = {};
    // Some bionic versions return 0 without touching the buffer when the property is
    // absent, so an initialised buffer and a checked length are both required.
    if (__system_property_get("ro.build.version.release", version) <= 0) {
        return 0;
    }
    int parsed = atoi(version);
    if (parsed <= 0) {
        return 0;
    }
    androidOsVersion.store(parsed, std::memory_order_relaxed);
    return parsed;
}

/// Extract `<package>` from a `<pid>:<package>/<uid>` token that ends at `tokenEnd`.
static std::string PackageFromPidToken(const std::string &buf, size_t tokenEnd) {
    if (tokenEnd == 0 || tokenEnd > buf.size()) {
        return {};
    }
    // rfind from tokenEnd - 1: the character at tokenEnd is usually the separating space.
    auto space = buf.rfind(' ', tokenEnd - 1);
    auto tokenStart = (space == std::string::npos) ? 0 : space + 1;
    auto colon = buf.find(':', tokenStart);
    if (colon == std::string::npos || colon >= tokenEnd) {
        return {};
    }
    auto pkgStart = colon + 1;
    auto slash = buf.find('/', pkgStart);
    auto pkgEnd = (slash == std::string::npos || slash > tokenEnd) ? tokenEnd : slash;
    if (pkgEnd <= pkgStart) {
        return {};
    }
    return buf.substr(pkgStart, pkgEnd - pkgStart);
}

std::string GetTopAppNameDumpsysAndroid7(void) {
    // venus:/ # dumpsys activity o | grep top-activity
    //     Proc # 3: fg     T/A/TOP  LCM  t: 0 30265:com.tencent.mm:toolsmp/u0a272 (top-activity)
    // venus:/ # dumpsys activity o | grep top-activity
    //     Proc # 0: fg     T/A/TOP  LCM  t: 0 2849:com.miui.home/u0a91 (top-activity)
    // osborn:/ # dumpsys activity o | grep top-activity
    //     Proc # 1: fore  T/A/TOP  trm: 0 2899:com.teslacoilsw.launcher/u0a178 (top-activity)
    std::string buf;
    ExecCmdSync(&buf, "/system/bin/dumpsys", "activity", "o");
    if (buf.empty()) {
        return {};
    }

    constexpr char TA_KEYWORD[] = " (top-activity)";
    auto keyword = buf.find(TA_KEYWORD);
    if (keyword == std::string::npos) {
        return {};
    }
    // The keyword starts with the space that ends the pid/package token.
    return PackageFromPidToken(buf, keyword);
}

std::string GetTopAppNameDumpsysAndroid10(void) {
    // matisse:/ # dumpsys activity lru | grep TOP
    // #101: fg     TOP  LCMN 21639:com.hengye.share/u0a351 act:activities|recents
    // #87: fg     BTOP ---N 4257:com.miui.securitycenter.remote/1000
    // #71: vis    BTOP ---N 6167:com.miui.securitycenter.bootaware/1000
    std::string buf;
    ExecCmdSync(&buf, "/system/bin/dumpsys", "activity", "lru");
    if (buf.empty()) {
        return {};
    }

    constexpr char TA_KEYWORD[] = " TOP";
    auto keyword = buf.find(TA_KEYWORD);
    if (keyword == std::string::npos) {
        return {};
    }
    auto colon = buf.find(':', keyword);
    if (colon == std::string::npos) {
        return {};
    }
    auto tokenEnd = buf.find_first_of(" \n", colon);
    if (tokenEnd == std::string::npos) {
        tokenEnd = buf.size();
    }
    return PackageFromPidToken(buf, tokenEnd);
}

std::string GetTopAppNameDumpsys(void) {
    auto ver = GetOSVersion();
    if (ver >= 10) {
        return GetTopAppNameDumpsysAndroid10(); // took ~40ms
    } else if (ver >= 7) {
        return GetTopAppNameDumpsysAndroid7(); // took ~100ms
    }
    return {};
}

std::string GetHomePackageName(void) {
    // Android 7+
    // /system/bin/cmd package resolve-activity -a android.intent.action.MAIN -c android.intent.category.HOME
    // Android 10+
    // /system/bin/pm resolve-activity -a android.intent.action.MAIN -c android.intent.category.HOME
    std::string buf;
    ExecCmdSync(&buf, "/system/bin/cmd", "package", "resolve-activity", "-a", "android.intent.action.MAIN", "-c",
                "android.intent.category.HOME");
    if (buf.empty()) {
        return {};
    }

    constexpr char KEYWORD[] = "packageName=";
    auto pos = buf.find(KEYWORD);
    if (pos == std::string::npos) {
        return {};
    }
    auto valueStart = pos + sizeof(KEYWORD) - 1; // no '\0'
    auto valueEnd = buf.find_first_of("\r\n", valueStart);
    if (valueEnd == std::string::npos) {
        valueEnd = buf.size();
    }
    return buf.substr(valueStart, valueEnd - valueStart);
}

std::string GetTombstone(int pid) {
    constexpr char TOMBSTONE_DIR[] = "/data/tombstones";
    constexpr int TOMBSTONE_HEAD_LINES = 8;
    // Tombstones can be several MB; the recovery path only needs the crash header + trace.
    constexpr std::size_t TOMBSTONE_MAX_BYTES = 512 * 1024;

    if (pid <= 0 || access(TOMBSTONE_DIR, F_OK) != 0) {
        return {};
    }

    char path[PATH_MAX];
    std::string latest;
    int64_t latestTs = 0;
    struct stat st;

    // get the latest tombstone
    DIR *tsfp = opendir(TOMBSTONE_DIR);
    if (tsfp == nullptr) {
        // access(F_OK) succeeding does not guarantee opendir() will (SELinux denial, fd
        // exhaustion, directory replaced in between) and readdir(nullptr) crashes.
        return {};
    }
    struct dirent *dp;
    while ((dp = readdir(tsfp))) {
        // skip . and ..
        if (dp->d_name[0] == '.') {
            continue;
        }
        snprintf(path, sizeof(path), "%s/%s", TOMBSTONE_DIR, dp->d_name);
        if (stat(path, &st) == 0) {
            // Integer arithmetic: `tv_sec * 1e9` promoted to double and lost the nanoseconds
            // (the product is far outside 2^53).
            auto ts = static_cast<int64_t>(st.st_mtim.tv_sec) * 1000000000LL + st.st_mtim.tv_nsec;
            if (ts > latestTs) {
                latest = path;
                latestTs = ts;
            }
        }
    }
    closedir(tsfp);

    if (latestTs == 0) {
        return {};
    }

    FILE *fp = fopen(latest.c_str(), "re");
    if (fp == NULL) {
        return {};
    }

    char buf[512];
    bool isThisFile = false;
    for (int i = 0; i < TOMBSTONE_HEAD_LINES; ++i) {
        // fgets() returning NULL leaves `buf` untouched, so sscanf() would parse the
        // previous iteration's line and could match the wrong pid.
        if (fgets(buf, sizeof(buf), fp) == nullptr) {
            break;
        }
        // pid: 17096, tid: 17098, name: unwind  >>> ./unwind <<<
        int owner_pid = 0;
        if (sscanf(buf, "%*s %d", &owner_pid) == 1 && owner_pid == pid) {
            isThisFile = true;
            break;
        }
    }
    if (isThisFile == false) {
        fclose(fp);
        return {};
    }

    std::string bt;
    rewind(fp);
    // Loop on the read result, not feof(): the old loop appended the last line twice and, on
    // a read error, appended stale data forever.
    while (fgets(buf, sizeof(buf), fp) != nullptr) {
        bt += buf;
        if (bt.size() >= TOMBSTONE_MAX_BYTES) {
            bt += "\n[uperf: tombstone truncated]\n";
            break;
        }
    }

    fclose(fp);
    return bt;
}

int GetScreenBrightness(void) {
    std::string buf;
    ExecCmdSync(&buf, "/system/bin/cmd", "settings", "get", "system", "screen_brightness");
    if (buf.empty()) {
        return -1;
    }

    int brightness = 0;
    if (ParseInt(buf.c_str(), &brightness)) {
        return brightness;
    }
    return -1;
}

void CallSettingsPut(const char *ns, const char *key, const char *val) {
    ExecCmd(nullptr, "/system/bin/cmd", "settings", "put", ns, key, val);
}

void SyncCallSurfaceflingerBackdoor(const char *code, const char *hz) {
    ExecCmdSync(nullptr, "/system/bin/service", "call", "SurfaceFlinger", code, "i32", hz);
}

void SysPeakRefreshRate(const std::string &hz) {
    // Validate here rather than in every caller: an empty/non-numeric value is written
    // straight into the system settings by `cmd settings put`.
    int parsed = 0;
    if (!ParseInt(hz.c_str(), &parsed) || parsed <= 0 || parsed > 240) {
        return;
    }
    CallSettingsPut("system", "peak_refresh_rate", hz.c_str());
    CallSettingsPut("system", "min_refresh_rate", hz.c_str());
    CallSettingsPut("system", "miui_refresh_rate", hz.c_str());
    CallSettingsPut("secure", "miui_refresh_rate", hz.c_str());
}

void SysSurfaceflingerBackdoor(int configIndex, bool force) {
    // >= Android 10
    // 1035 -1/0/1/2: setActiveConfig
    char idx[16];
    snprintf(idx, sizeof(idx), "%d", configIndex);
    SyncCallSurfaceflingerBackdoor("1035", idx);

    if (force) {
        // >= Android 11
        // 1036 1: Frame rate flexibility token acquired. count=1
        // 1036 0: Frame rate flexibility token released. count=0
        // service call SurfaceFlinger 1035 i32 -1 -- okay
        // service call SurfaceFlinger 1036 i32 1
        // service call SurfaceFlinger 1035 i32 2 -- not working
        SyncCallSurfaceflingerBackdoor("1036", "1");
        SyncCallSurfaceflingerBackdoor("1035", "-1");
        SyncCallSurfaceflingerBackdoor("1036", "0");
        SyncCallSurfaceflingerBackdoor("1035", idx);
    }
}
