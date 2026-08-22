#pragma once

#include <cstdlib>
#include <functional>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

/// A minimal test harness.
///
/// Hand-written for the same reason the library has no dependencies: pulling a
/// framework in through FetchContent would mean every CI run, on every platform,
/// depends on a download that has nothing to do with what is being tested.
namespace harness {

struct Case {
    std::string name;
    std::function<void()> body;
};

inline std::vector<Case>& registry() {
    static std::vector<Case> cases;
    return cases;
}

struct Registrar {
    Registrar(std::string name, std::function<void()> body) {
        registry().push_back({std::move(name), std::move(body)});
    }
};

struct Failure {
    std::string message;
};

inline void fail(const std::string& file, int line, const std::string& message) {
    std::ostringstream out;
    out << file << ":" << line << "  " << message;
    throw Failure{out.str()};
}

inline int run(const char* suite) {
    int failed = 0;
    for (const auto& test : registry()) {
        try {
            test.body();
        } catch (const Failure& failure) {
            ++failed;
            std::cout << "FAIL  " << test.name << "\n      " << failure.message << "\n";
        } catch (const std::exception& error) {
            ++failed;
            std::cout << "FAIL  " << test.name << "\n      threw: " << error.what() << "\n";
        }
    }
    std::cout << suite << ": " << (registry().size() - static_cast<std::size_t>(failed)) << "/"
              << registry().size() << " passed\n";
    return failed == 0 ? 0 : 1;
}

}  // namespace harness

#define AURU_CONCAT_INNER(a, b) a##b
#define AURU_CONCAT(a, b) AURU_CONCAT_INNER(a, b)

#define TEST(name)                                                            \
    static void AURU_CONCAT(auru_test_, __LINE__)();                          \
    static const ::harness::Registrar AURU_CONCAT(auru_registrar_, __LINE__)( \
        name, AURU_CONCAT(auru_test_, __LINE__));                             \
    static void AURU_CONCAT(auru_test_, __LINE__)()

#define CHECK(condition)                                                  \
    do {                                                                  \
        if (!(condition)) {                                               \
            ::harness::fail(__FILE__, __LINE__, "expected " #condition);  \
        }                                                                 \
    } while (false)

// Deliberately by value, not by reference. `Result::value()` returns a
// reference into the Result, and binding `const auto&` to that when the Result
// itself is a temporary leaves a dangling reference — lifetime extension does
// not reach through a function call. Copying costs nothing here and removes a
// way for a test to report garbage instead of a failure.
#define CHECK_EQ(actual, expected)                                          \
    do {                                                                    \
        const auto auru_actual = (actual);                                  \
        const auto auru_expected = (expected);                              \
        if (!(auru_actual == auru_expected)) {                              \
            std::ostringstream auru_out;                                    \
            auru_out << "expected " << auru_expected << "\n           got " \
                     << auru_actual;                                        \
            ::harness::fail(__FILE__, __LINE__, auru_out.str());            \
        }                                                                   \
    } while (false)

#define TEST_MAIN(suite)                       \
    int main() { return ::harness::run(suite); }
