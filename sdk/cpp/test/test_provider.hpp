#pragma once

#include <csignal>
#include <spawn.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>

#include "socket_transport.hpp"

/// A real `auru-pm-server`, for the end-to-end tests.
///
/// Mocked HTTP would only prove this client agrees with the test's idea of the
/// protocol. Running the reference implementation is the only way to find out
/// whether the two interoperate.
class TestProvider {
public:
    TestProvider() {
        const std::string repo = AURU_REPO_ROOT;
        std::system(("cargo build -p auru-pm-server --locked --manifest-path " + repo
                     + "/Cargo.toml >/dev/null 2>&1")
                        .c_str());

        char directory_template[] = "/tmp/auru-cpp-XXXXXX";
        directory_ = ::mkdtemp(directory_template);
        port_ = 4600 + (::getpid() % 300);

        const std::string config = directory_ + "/server.toml";
        std::ofstream out(config);
        out << "version = 1\n"
            << "provider_id = \"cpp-sdk-test\"\n"
            << "listen = \"127.0.0.1:" << port_ << "\"\n"
            << "data_dir = \"" << directory_ << "/data\"\n"
            << "requests_per_minute = 100000\n\n"
            << "[authentication]\nmode = \"none\"\n";
        out.close();

        const std::string command = repo + "/target/debug/auru-pm-server --config " + config
                                    + " >/dev/null 2>&1 & echo $!";
        FILE* pipe = ::popen(command.c_str(), "r");
        if (pipe != nullptr) {
            char line[32] = {0};
            if (std::fgets(line, sizeof(line), pipe) != nullptr) {
                pid_ = std::atoi(line);
            }
            ::pclose(pipe);
        }

        SocketTransport transport;
        for (int attempt = 0; attempt < 200; ++attempt) {
            auru::pm::Request probe;
            probe.method = "GET";
            probe.url = endpoint() + "/v1/health";
            auto response = transport.send(probe);
            if (response.has_value() && response.value().status == 200) {
                return;
            }
            ::usleep(50000);
        }
        std::fprintf(stderr, "the provider did not start on %s\n", endpoint().c_str());
    }

    ~TestProvider() {
        if (pid_ > 0) {
            ::kill(pid_, SIGTERM);
        }
        std::system(("rm -rf " + directory_).c_str());
    }

    TestProvider(const TestProvider&) = delete;
    TestProvider& operator=(const TestProvider&) = delete;

    std::string endpoint() const { return "http://127.0.0.1:" + std::to_string(port_); }

private:
    std::string directory_;
    int port_ = 0;
    int pid_ = 0;
};
