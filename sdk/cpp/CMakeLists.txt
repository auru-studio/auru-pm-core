cmake_minimum_required(VERSION 3.16)

project(auru_pm
    VERSION 0.1.0
    DESCRIPTION "Client for auru-pm-v1 project-management providers"
    LANGUAGES CXX)

# C++17: where JUCE and most audio codebases sit. Nothing here needs newer, and
# `std::expected` is C++23 anyway, so the result type is ours either way.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

option(AURU_PM_BUILD_TESTS "Build the test suite" ${PROJECT_IS_TOP_LEVEL})
option(AURU_PM_WITH_CURL "Build the optional libcurl transport adapter" OFF)

# The core has no dependencies at all — not even an HTTP client. Transport is an
# interface the caller supplies, because most applications that would use this
# already have an HTTP stack and forcing a second one on them is rude.
add_library(auru_pm
    src/blake3.cpp
    src/content_hash.cpp
    src/json.cpp
    src/error.cpp
    src/commit.cpp
    src/types.cpp
    src/client.cpp
    src/sha256.cpp
    src/oauth.cpp)
add_library(auru::pm ALIAS auru_pm)
# So the installed target is `auru::pm` too, rather than `auru::auru_pm` — the
# alias above exists only in this build tree, and a consumer that links the name
# from the README would otherwise get a "target not found" from the install.
set_target_properties(auru_pm PROPERTIES EXPORT_NAME pm)

target_include_directories(auru_pm
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:include>)

target_compile_features(auru_pm PUBLIC cxx_std_17)

if(MSVC)
    target_compile_options(auru_pm PRIVATE /W4 /WX /permissive-)
else()
    target_compile_options(auru_pm PRIVATE
        -Wall -Wextra -Wpedantic -Werror
        -Wconversion -Wsign-conversion -Wshadow -Wold-style-cast)
endif()

# The adapter is a separate target so linking the core never drags in libcurl.
if(AURU_PM_WITH_CURL)
    find_package(CURL REQUIRED)
    add_library(auru_pm_curl src/curl_transport.cpp)
    add_library(auru::pm_curl ALIAS auru_pm_curl)
    set_target_properties(auru_pm_curl PROPERTIES EXPORT_NAME pm_curl)
    target_link_libraries(auru_pm_curl PUBLIC auru_pm PRIVATE CURL::libcurl)
    target_compile_features(auru_pm_curl PUBLIC cxx_std_17)
endif()

if(AURU_PM_BUILD_TESTS)
    enable_testing()
    add_subdirectory(test)
endif()

include(GNUInstallDirs)

set(AURU_PM_INSTALL_TARGETS auru_pm)
if(AURU_PM_WITH_CURL)
    list(APPEND AURU_PM_INSTALL_TARGETS auru_pm_curl)
endif()

install(TARGETS ${AURU_PM_INSTALL_TARGETS} EXPORT auru_pmTargets
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}
    LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(DIRECTORY include/ DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(EXPORT auru_pmTargets
    FILE auru_pmTargets.cmake
    NAMESPACE auru::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/auru_pm)

# Without these a consumer's `find_package(auru_pm)` fails with a message about
# a missing config file, which is a confusing way to learn that an install is
# incomplete.
include(CMakePackageConfigHelpers)
write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/auru_pmConfigVersion.cmake"
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion)
configure_package_config_file(
    cmake/auru_pmConfig.cmake.in
    "${CMAKE_CURRENT_BINARY_DIR}/auru_pmConfig.cmake"
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/auru_pm)
install(FILES
    "${CMAKE_CURRENT_BINARY_DIR}/auru_pmConfig.cmake"
    "${CMAKE_CURRENT_BINARY_DIR}/auru_pmConfigVersion.cmake"
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/auru_pm)
