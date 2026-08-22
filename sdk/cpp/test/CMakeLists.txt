# The repository root, so tests can read the published vectors and start the
# reference server.
set(AURU_REPO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../..")
get_filename_component(AURU_REPO_ROOT "${AURU_REPO_ROOT}" ABSOLUTE)

function(auru_add_test name)
    add_executable(${name} ${name}.cpp)
    target_link_libraries(${name} PRIVATE auru_pm)
    target_include_directories(${name} PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${CMAKE_CURRENT_SOURCE_DIR}/../src)
    target_compile_definitions(${name} PRIVATE AURU_REPO_ROOT="${AURU_REPO_ROOT}")
    if(NOT MSVC)
        target_compile_options(${name} PRIVATE -Wall -Wextra -Werror)
    endif()
    add_test(NAME ${name} COMMAND ${name})
endfunction()

auru_add_test(blake3_test)
auru_add_test(commit_vectors_test)
auru_add_test(json_test)
auru_add_test(transport_test)
auru_add_test(error_mapping_test)
auru_add_test(oauth_test)
