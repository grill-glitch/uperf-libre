cmake_minimum_required(VERSION 3.13)

project(uperf-libre LANGUAGES C CXX)

# The vendored thirdparty tree (scnlib) declares cmake_minimum_required(VERSION 3.4),
# which CMake >= 4.0 refuses to configure. This is the documented escape hatch; it does
# NOT modify the vendored files (see cpp/dfps/DFPS_VENDOR.md).
set(CMAKE_POLICY_VERSION_MINIMUM 3.5 CACHE STRING "policy floor for vendored deps (scnlib 3.4)")

set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/runnable)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/runnable)

add_subdirectory(cpp)
