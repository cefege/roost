cmake_minimum_required(VERSION 3.24)
cmake_policy(SET CMP0091 NEW)
project(roost_win_helper LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "roost-win-helper must be built with MSVC for Windows")
endif()
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "roost-win-helper is Windows x64 only")
endif()

# This helper ships beside the standalone Bun executables, so it must not depend
# on the redistributable CRT. Keep every configuration statically linked.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded" CACHE STRING "" FORCE)

add_executable(roost-win-helper roost-win-helper.cpp)
target_compile_features(roost-win-helper PRIVATE cxx_std_20)
target_compile_definitions(roost-win-helper PRIVATE
  UNICODE
  _UNICODE
  WIN32_LEAN_AND_MEAN
  NOMINMAX
  _WIN32_WINNT=0x0A00
)
target_compile_options(roost-win-helper PRIVATE /W4 /WX /permissive- /utf-8 /sdl /guard:cf)
target_link_options(roost-win-helper PRIVATE /guard:cf /DYNAMICBASE /NXCOMPAT)
target_link_libraries(roost-win-helper PRIVATE
  advapi32
  bcrypt
  crypt32
  iphlpapi
  shell32
  userenv
  wintrust
  ws2_32
)
set_target_properties(roost-win-helper PROPERTIES
  OUTPUT_NAME "roost-win-helper"
  SUFFIX ".exe"
)
