cmake_minimum_required(VERSION 3.16)
project(auru_pm_consumer_check LANGUAGES CXX)

# Consume the installed package the way anyone else would: find_package, link
# `auru::pm`, and nothing else. If a header were missing from the install rules
# or the core had picked up a hidden dependency, this is where it shows.
find_package(auru_pm REQUIRED)

add_executable(consumer_check main.cpp)
target_link_libraries(consumer_check PRIVATE auru::pm)
target_compile_features(consumer_check PRIVATE cxx_std_17)
