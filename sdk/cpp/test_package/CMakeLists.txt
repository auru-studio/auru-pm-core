cmake_minimum_required(VERSION 3.16)
project(auru_pm_test_package LANGUAGES CXX)

find_package(auru_pm REQUIRED CONFIG)

add_executable(example example.cpp)
target_link_libraries(example PRIVATE auru::pm)
target_compile_features(example PRIVATE cxx_std_17)
