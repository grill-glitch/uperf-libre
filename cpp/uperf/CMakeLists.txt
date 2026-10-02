# uperf — Rust policy engine (M1+) linked against the vendored dfps C++ platform.
#
# Source policy (see AGENT.md §3, §4):
#   cpp/dfps/source/**   vendored, byte-identical to upstream dfps, NOT modified here.
#                        We only *select* files: dfps' own main.cpp, dfps.cpp/h and
#                        dynamic_fps.{h,cpp} (dfps' business layer) are excluded.
#   cpp/uperf/**         this project: uperf app assembly + (M1) the Rust bridge.

cmake_minimum_required(VERSION 3.13)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_EXTENSIONS OFF)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_EXTENSIONS OFF)

set(DFPS_SOURCE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../dfps/source)

set(UPERF_SRCS
    ${CMAKE_CURRENT_SOURCE_DIR}/app_main.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/m0_event_tap.cpp

    ${DFPS_SOURCE_DIR}/platform/cobridge.cpp
    ${DFPS_SOURCE_DIR}/platform/delayed_worker.cpp
    ${DFPS_SOURCE_DIR}/platform/heavy_worker.cpp
    ${DFPS_SOURCE_DIR}/platform/inotifier.cpp
    ${DFPS_SOURCE_DIR}/platform/module_base.cpp
    ${DFPS_SOURCE_DIR}/modules/cgroup_listener.cpp
    ${DFPS_SOURCE_DIR}/modules/input_listener.cpp
    ${DFPS_SOURCE_DIR}/modules/offscreen_monitor.cpp
    ${DFPS_SOURCE_DIR}/modules/topapp_monitor.cpp
    ${DFPS_SOURCE_DIR}/utils/atrace.c
    ${DFPS_SOURCE_DIR}/utils/frame_pointer_trace.c
    ${DFPS_SOURCE_DIR}/utils/inotify.cpp
    ${DFPS_SOURCE_DIR}/utils/input_reader.cpp
    ${DFPS_SOURCE_DIR}/utils/misc.cpp
    ${DFPS_SOURCE_DIR}/utils/misc_android.cpp
    ${DFPS_SOURCE_DIR}/utils/sched_ctrl.c
    ${DFPS_SOURCE_DIR}/utils/time_counter.cpp
)

set(UPERF_INCS
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../include
    ${CMAKE_CURRENT_SOURCE_DIR}/../dfps/source
)

# version.c from the vendored template (same recipe as dfps' source/CMakeLists.txt)
execute_process(COMMAND git log -1 --format=%h
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
    OUTPUT_VARIABLE GIT_REPO_HASH
    OUTPUT_STRIP_TRAILING_WHITESPACE
)
configure_file(${DFPS_SOURCE_DIR}/version.c.in ${CMAKE_CURRENT_BINARY_DIR}/version.c @ONLY)
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS ${CMAKE_SOURCE_DIR}/.git/index)

add_executable(uperf ${UPERF_SRCS} ${CMAKE_CURRENT_BINARY_DIR}/version.c)
target_include_directories(uperf PRIVATE ${UPERF_INCS})
target_link_libraries(uperf PRIVATE spdlog scn)

set(THIS_COMPILE_FLAGS
    -Wall -Werror -fdata-sections -ffunction-sections -fno-threadsafe-statics -fno-omit-frame-pointer -fno-rtti
    -D_GNU_SOURCE
)

# -ffixed-x18, --hash-style=both: copied from dfps so the loader behaviour matches the
# original uperf v3 binary.
# NOTE: unlike dfps Release, we deliberately do NOT pass -static: the original
# magisk/bin/uperf is a *dynamic* PIE whose NEEDED list is exactly libm/libdl/libc
# (dfps' own build is fully static). Keeping dynamic libc + static libc++ reproduces
# the original's ELF shape, which build.sh `check` asserts.
set(THIS_LINK_FLAGS
    -ffixed-x18 -Wl,--hash-style=both -fPIE -Wl,-exclude-libs,ALL -Wl,--gc-sections
)

if(CMAKE_BUILD_TYPE STREQUAL "Release")
    target_link_libraries(uperf PRIVATE c++_static dl m)
    # -static-libstdc++: link libc++ statically while keeping the dynamic libc/libm/libdl
    # (the driver otherwise defaults to libc++_shared, which does not exist on Android).
    # The original uperf v3 binary has exactly NEEDED libm/libdl/libc — see build.sh check.
    list(APPEND THIS_COMPILE_FLAGS -O3 -fvisibility=hidden -fvisibility-inlines-hidden -flto)
    list(APPEND THIS_LINK_FLAGS -static-libstdc++ -O3 -fvisibility=hidden -fvisibility-inlines-hidden -flto
        -Wl,--icf=all,-O3,--lto-O3,--strip-all)
elseif(CMAKE_BUILD_TYPE STREQUAL "Debug")
    target_link_libraries(uperf PRIVATE c++_shared dl m)
    list(APPEND THIS_COMPILE_FLAGS -fsanitize=address -DSPDLOG_ACTIVE_LEVEL=SPDLOG_LEVEL_DEBUG -g)
    list(APPEND THIS_LINK_FLAGS -fsanitize=address)
endif()

target_compile_options(uperf PRIVATE ${THIS_COMPILE_FLAGS})
target_link_options(uperf PRIVATE ${THIS_LINK_FLAGS})
