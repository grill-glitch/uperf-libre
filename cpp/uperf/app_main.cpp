/*
 * Copyright (C) 2021-2022 Matt Yang
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

// uperf process entry point, derived from the vendored dfps `source/main.cpp`
// (Apache-2.0, commit f84866c1 — see cpp/dfps/DFPS_VENDOR.md).
//
// Reused from dfps verbatim: the process supervisor (InitLogger / PrintTombstone /
// StartNewApp / KillOldApp / DaemonSigHandler / Daemon / main), the getopt handling
// pattern, and the log pattern "%H:%M:%S %L %v" (byte-identical in the original
// uperf v3 binary, so log lines stay parseable by the same tooling).
//
// Changed for uperf: PROC_NAME / VERSION / AUTHOR / HELP_DESC, and the app assembly.
// M0 (this file): bring up the vendored platform plus the four event sources and prove
// the event pipeline on a real device — no policy is applied yet.
// M1: StartPlatform() is replaced by uperf_rs_start() from the Rust staticlib
// (cpp/include/uperf_rs.h).

#include <getopt.h>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>
#include <sys/wait.h>
#include <unistd.h>
#include <vector>

#include <spdlog/sinks/basic_file_sink.h>
#include <spdlog/spdlog.h>

#include "m0_event_tap.h"
#include "modules/cgroup_listener.h"
#include "modules/input_listener.h"
#include "modules/offscreen_monitor.h"
#include "modules/topapp_monitor.h"
#include "platform/heavy_worker.h"
#include "platform/module_base.h"
#include "uperf_rs_bridge.h"
#include "utils/inotify.h"
#include "utils/misc.h"
#include "utils/misc_android.h"
#include "utils/sched_ctrl.h"
#include "version.h"

static constexpr char PROC_NAME[] = "uperf";
static constexpr char AUTHOR[] = "grill-glitch (Rust rewrite project)";
static constexpr char VERSION[] = "m0(rs-rewrite)";
// The help text is the original uperf v3 string (verified present in the upstream
// dev-22.09.04 binary), because magisk/script/libuperf.sh and user tooling rely on it.
static constexpr char HELP_DESC[] =
    "Userspace performance controller for Android 6.0+. Details see https://github.com/yc9559/uperf.\n"
    "Usage: uperf [-o log_file] config_file";
static constexpr int TERM_SIG = SIGUSR1;

static std::string configFile;
static std::string logFile;

static pid_t dead_pid;
static pid_t new_pid;
static pid_t old_pid;

// ---------------------------------------------------------------- logger

static void InitLogger(void) {
    auto logger = spdlog::default_logger();
    if (logFile.empty() == false) {
        auto sink = std::make_shared<spdlog::sinks::basic_file_sink_mt>(logFile, false);
        logger->sinks().emplace_back(sink);
    }
    logger->set_pattern("%H:%M:%S %L %v");
    // Default only: the Rust side applies `modules.log.level` once the config is
    // parsed (all 63 shipped configs say "info"). This used to be hardcoded to
    // debug.
    logger->set_level(spdlog::level::info);
    logger->flush_on(spdlog::level::info);
}

static void PrintTombstone(int pid) {
    auto bt = GetTombstone(pid);
    if (bt.empty()) {
        SPDLOG_ERROR("Cannot find the tombstone");
        return;
    }

    SPDLOG_ERROR(">>> Start of tombstone {} <<<", pid);
    std::stringstream ss(bt);
    std::string line;
    while (std::getline(ss, line)) {
        SPDLOG_ERROR(line);
    }
    SPDLOG_ERROR(">>> End of tombstone {} <<<", pid);
}

// ---------------------------------------------------------------- app

// M0: platform bring-up only. This mirrors the module assembly of the vendored dfps
// (`Dfps::Start()`), minus dfps' business module (DynamicFps) — uperf's equivalent
// modules are the Rust ones from M1 on.
//
// The event tap must start before CgroupListener: CgroupListener::Updater::Start() only
// registers its work when the topic already has a subscriber (`CoHasSubscriber`), so the
// presence of the tap is what activates the cgroup listeners at all.
static void StartPlatform(void) {
    // same sched hint as dfps: heavy worker gets 120, the rest 98
    SchedCtrlSetStaticPrio(0, 120, false);
    HeavyWorker::GetInstance();
    SchedCtrlSetStaticPrio(0, 98, false);

    static std::vector<std::unique_ptr<ModuleBase>> modules;
    modules.emplace_back(std::make_unique<M0EventTap>());
    // Register before pushing: Rust applies `modules.input.*` to this instance
    // once it has parsed the config (the listener's thresholds are hardcoded in
    // the vendored dfps ctor).
    {
        auto inputListener = std::make_unique<InputListener>();
        uperf_register_input_listener(inputListener.get());
        modules.emplace_back(std::move(inputListener));
    }
    modules.emplace_back(std::make_unique<CgroupListener>());
    modules.emplace_back(std::make_unique<TopappMonitor>());
    modules.emplace_back(std::make_unique<OffscreenMonitor>());

    for (const auto &m : modules) {
        m->Start();
    }
}

static void AppMainMayThrow(void) {
    SPDLOG_INFO("{}[{}] M1 platform bring-up, config={} log={} (Rust event tap)",
                PROC_NAME, GetGitCommitHash(), configFile, logFile);
    StartPlatform();
    SPDLOG_INFO("Uperf is running");
    // Hand off to the Rust engine. From this point on, the Rust dispatcher
    // thread logs every event the C++ bridge forwards across `uperf_rs_on_event`.
    uperf_bridge_init_rust(configFile.c_str(), logFile.c_str());
    SPDLOG_INFO("uperf_bridge_init_rust returned, entering main loop");
    for (;;) {
        Sleep(UINT32_MAX);
    }
}

static void AppMain(void) {
    try {
        AppMainMayThrow();
    } catch (const std::exception &e) {
        SPDLOG_ERROR("Exception thrown: {}", e.what());
        exit(EXIT_FAILURE);
    }
}

// ---------------------------------------------------------------- supervisor (dfps)

static void AppSigHandler(int sig) {
    // Block the shutdown signals for the duration of the handler.
    //
    // Without this the handler re-enters: `killall uperf` delivers SIGTERM to the
    // daemon AND the worker at the same time, and the daemon's own handler then
    // also sends SIGUSR1 to the worker. Two concurrent runs of the shutdown path
    // fight over the same non-reentrant log mutex (Rust's global line buffer and
    // spdlog's sink lock), deadlock, and the process never exits — leaving the CPU
    // governor armed in `userspace` mode. Observed on device: `killall uperf` left
    // two processes alive with `governors: userspace userspace userspace`.
    sigset_t block;
    sigemptyset(&block);
    sigaddset(&block, TERM_SIG);
    sigaddset(&block, SIGTERM);
    sigaddset(&block, SIGINT);
    sigprocmask(SIG_BLOCK, &block, nullptr);

    switch (sig) {
        case TERM_SIG:
        case SIGTERM:
        case SIGINT:
            // Stop the Rust engine before exiting. The CPU governor runs in
            // "userspace" mode, i.e. it has taken frequency scaling away from the
            // kernel; if we exit without uperf_rs_stop() the policies stay pinned
            // to the last published frequency forever. uperf_rs_stop() disarms
            // them.
            //
            // SIGTERM/SIGINT must be handled here, not only TERM_SIG: the worker
            // inherits the *daemon's* SIGTERM handler (SetSigHandler only replaced
            // TERM_SIG), and the daemon's handler just exits — so a plain
            // `killall uperf`, which is exactly what the module's own
            // `uperf_stop()` does, killed the worker without disarming the
            // governor. uperf_rs_stop() is idempotent, so a daemon-then-worker
            // sequence is fine.
            uperf_rs_stop();
            exit(EXIT_SUCCESS);
        default:
            exit(EXIT_FAILURE);
    }
}

static void SetSigHandler(void) {
    signal(TERM_SIG, AppSigHandler);
    // Replace the inherited daemon handler: a direct SIGTERM to a worker (e.g.
    // `killall uperf`) must run the same cleanup, not the supervisor's exit path.
    signal(SIGTERM, AppSigHandler);
    signal(SIGINT, AppSigHandler);
}

static void StartNewApp(void) {
    new_pid = fork();
    if (new_pid == 0) {
        SetSigHandler();
        AppMain();
    }

    Sleep(SToUs(1.0));
    if (new_pid != dead_pid) {
        old_pid = new_pid;
    } else {
        SPDLOG_INFO("Failed to start {}(pid={})", PROC_NAME, new_pid);
    }
}

static void KillOldApp(void) {
    if (old_pid) {
        kill(old_pid, TERM_SIG);
    }
}

static void DaemonSigHandler(int signum) {
    pid_t child_pid;
    int status;
    switch (signum) {
        case SIGCHLD:
            child_pid = wait(&status);
            if (child_pid != new_pid && child_pid != old_pid) {
                return;
            }
            dead_pid = child_pid;
            if (WIFSIGNALED(status)) {
                SPDLOG_ERROR("{}(pid={}) terminated unexpectedly, try to get tombstone", PROC_NAME, dead_pid);
                // wait tombstone generated
                Sleep(SToUs(1.0));
                PrintTombstone(dead_pid);
            }
            break;
        case SIGTERM:
        case SIGINT:
            // Take the worker down with us. dfps' original handler just exited,
            // which orphaned the (forked) worker — and an orphaned worker keeps
            // the CPU governor armed, leaving every policy stuck in `userspace`.
            SPDLOG_INFO("Terminated by user, stopping worker");
            if (new_pid) {
                kill(new_pid, TERM_SIG);
            }
            if (old_pid && old_pid != new_pid) {
                kill(old_pid, TERM_SIG);
            }
            Sleep(SToUs(0.5)); // let the worker run uperf_rs_stop()
            exit(EXIT_SUCCESS);
        default:
            break;
    }
}

static void Daemon(void) {
    signal(SIGCHLD, DaemonSigHandler);
    signal(SIGTERM, DaemonSigHandler);
    signal(SIGINT, DaemonSigHandler);

    SPDLOG_INFO("{} {}[{}], by {}", PROC_NAME, VERSION, GetGitCommitHash(), AUTHOR);

    Inotify inotify;
    inotify.Add(configFile, Inotify::CLOSE_WRITE, nullptr);

    StartNewApp();

    for (;;) {
        inotify.WaitAndHandle();
        SPDLOG_INFO("Config file updated, restart {} to load new config file", PROC_NAME);
        KillOldApp();
        Sleep(SToUs(0.5)); // wait finishing termination
        StartNewApp();
    }
}

// ---------------------------------------------------------------- cli (dfps)

static void PrintHelp(void) { std::cout << std::endl << HELP_DESC << std::endl; }

static void ParseOpt(int argc, char **argv) {
    static const char opt_string[] = "ho:";
    static const struct option long_opts[] = {
        {"help", no_argument, NULL, 'h'},
        {"outfile", required_argument, NULL, 'o'},
        {NULL, 0, 0, 0},
    };
    int opt;
    while ((opt = getopt_long(argc, argv, opt_string, long_opts, NULL)) != -1) {
        switch (opt) {
            case 'h':
                PrintHelp();
                exit(EXIT_SUCCESS);
                break;
            case 'o':
                logFile = std::string(optarg);
                remove(logFile.c_str());
                break;
            default:
                PrintHelp();
                exit(EXIT_FAILURE);
                break;
        }
    }
    int len = argc - optind;
    if (len < 1) {
        SPDLOG_ERROR("Config file not specified");
        std::cout << std::endl << HELP_DESC << std::endl;
        exit(EXIT_FAILURE);
    } else {
        configFile = argv[optind];
        if (access(configFile.c_str(), R_OK) != 0) {
            SPDLOG_ERROR("Config file not found");
            exit(EXIT_FAILURE);
        }
    }
}

int main(int argc, char **argv) {
    InitLogger();
    InitArgv(argc, argv);
    ParseOpt(argc, argv);
    InitLogger();

    pid_t pid = fork();
    if (pid == 0) {
        setsid();
        SetSelfName(PROC_NAME);
        Daemon();
    }
    return EXIT_SUCCESS;
}
