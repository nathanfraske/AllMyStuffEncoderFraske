# Used only for the target dependency build, never the Windows host tools.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR riscv64)

# cmake-rs 0.1.58 deliberately omits automatic compiler selection on a
# Windows host with a non-MSVC target. Set the complete cross toolchain here.
set(CMAKE_C_COMPILER "${CMAKE_CURRENT_LIST_DIR}/zig-cc.cmd")
set(CMAKE_CXX_COMPILER "${CMAKE_CURRENT_LIST_DIR}/zig-cxx.cmd")
set(CMAKE_ASM_COMPILER "${CMAKE_CURRENT_LIST_DIR}/zig-cc.cmd")
set(CMAKE_AR "${CMAKE_CURRENT_LIST_DIR}/zig-ar.cmd")
set(CMAKE_RANLIB "${CMAKE_CURRENT_LIST_DIR}/zig-ranlib.cmd")
set(CMAKE_C_COMPILER_AR "${CMAKE_AR}")
set(CMAKE_C_COMPILER_RANLIB "${CMAKE_RANLIB}")
set(CMAKE_CXX_COMPILER_AR "${CMAKE_AR}")
set(CMAKE_CXX_COMPILER_RANLIB "${CMAKE_RANLIB}")

# Ninja is supplied by the caller; do not select a Windows MSBuild generator.
file(TO_CMAKE_PATH "$ENV{ALLMYSTUFF_RISCV_NINJA}" _riscv_ninja)
set(CMAKE_MAKE_PROGRAM "${_riscv_ninja}" CACHE FILEPATH "Reviewed Ninja executable")

# Keep CMake's real compile/link probes. Do not force compiler success,
# pre-answer capability checks, or run target programs during configuration.
