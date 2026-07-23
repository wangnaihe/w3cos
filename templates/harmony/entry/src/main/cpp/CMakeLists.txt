cmake_minimum_required(VERSION 3.5.0)
project(w3cos_harmony_host)

set(NATIVERENDER_ROOT_PATH ${CMAKE_CURRENT_SOURCE_DIR})

include_directories(${NATIVERENDER_ROOT_PATH})

add_library(w3cos_harmony_host SHARED napi_init.cpp)

target_link_libraries(
  w3cos_harmony_host PUBLIC
  libace_napi.z.so
  libace_ndk.z.so
  libhilog_ndk.z.so
)
