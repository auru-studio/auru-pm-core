// What a consumer actually writes, built against the installed package with
// only `auru::pm` linked.
//
// That link line is the point. The library claims no dependencies, and the only
// way to know is to give it nothing else and see whether it works. The transport
// below is the caller's to provide — C++ has no standard HTTP client, so this
// library takes an interface rather than choosing one for you.

#include <netdb.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstring>
#include <fstream>
#include <iostream>
#include <memory>
#include <string>
#include <vector>

#include "auru/pm/client.hpp"

using namespace auru::pm;

namespace {

/// A minimal plaintext HTTP transport, enough to talk to a provider on
/// loopback. A real application would wrap whatever stack it already has, or
/// link `auru::pm_curl`.
class TinyTransport final : public Transport {
public:
    Result<Response> send(const Request& request) override {
        const auto scheme_end = request.url.find("://");
        const auto path_start = request.url.find('/', scheme_end + 3);
        const std::string authority =
            request.url.substr(scheme_end + 3, path_start - scheme_end - 3);
        const std::string path = request.url.substr(path_start);
        const auto colon = authority.rfind(':');
        const std::string host = authority.substr(0, colon);
        const std::string port = authority.substr(colon + 1);

        addrinfo hints{};
        hints.ai_family = AF_UNSPEC;
        hints.ai_socktype = SOCK_STREAM;
        addrinfo* candidates = nullptr;
        if (getaddrinfo(host.c_str(), port.c_str(), &hints, &candidates) != 0) {
            return make_error<Response>(ErrorCode::Internal, "cannot resolve " + host);
        }
        const int socket_fd =
            socket(candidates->ai_family, candidates->ai_socktype, candidates->ai_protocol);
        if (socket_fd < 0 || connect(socket_fd, candidates->ai_addr, candidates->ai_addrlen) != 0) {
            freeaddrinfo(candidates);
            return make_error<Response>(ErrorCode::Internal, "cannot connect to " + authority);
        }
        freeaddrinfo(candidates);

        std::string wire = request.method + " " + path + " HTTP/1.1\r\nHost: " + host
                           + "\r\nConnection: close\r\n";
        for (const auto& header : request.headers) {
            wire += header.first + ": " + header.second + "\r\n";
        }
        wire += "Content-Length: " + std::to_string(request.body.size()) + "\r\n\r\n";
        wire.append(reinterpret_cast<const char*>(request.body.data()), request.body.size());
        if (write(socket_fd, wire.data(), wire.size()) < 0) {
            close(socket_fd);
            return make_error<Response>(ErrorCode::Internal, "the connection closed while writing");
        }

        std::string raw;
        char buffer[8192];
        ssize_t got = 0;
        while ((got = read(socket_fd, buffer, sizeof(buffer))) > 0) {
            raw.append(buffer, static_cast<std::size_t>(got));
        }
        close(socket_fd);

        Response response;
        const auto header_end = raw.find("\r\n\r\n");
        response.status = std::stoi(raw.substr(raw.find(' ') + 1, 3));
        const std::string body = raw.substr(header_end + 4);
        response.body.assign(body.begin(), body.end());
        return Result<Response>::ok(std::move(response));
    }
};

std::vector<std::uint8_t> bytes_of(const std::string& text) {
    return std::vector<std::uint8_t>(text.begin(), text.end());
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 3) {
        std::cerr << "usage: consumer_check <endpoint> <repo-root>\n";
        return 2;
    }
    const std::string endpoint = argv[1];
    const std::string repo_root = argv[2];

    auto client = AuruClient::connect({endpoint, std::make_shared<TinyTransport>(), std::nullopt});
    if (!client) {
        std::cerr << "connect: " << client.error().to_string() << "\n";
        return 1;
    }
    std::cout << "client protocol:  " << kProtocolVersion << "\n";
    std::cout << "connected to:     " << client.value().health().provider_id.value_or("?")
              << " at " << client.value().endpoint() << "\n";

    auto identity = client.value().me();
    auto project = client.value().project("demo/night-drive");

    ProjectProfile profile;
    profile.display_name = "Night Drive";
    profile.format = "dawproject";
    if (auto stored = project.value().put_profile(profile); !stored) {
        std::cerr << "put_profile: " << stored.error().to_string() << "\n";
        return 1;
    }

    std::ifstream file(
        repo_root + "/crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject",
        std::ios::binary);
    const std::vector<std::uint8_t> snapshot((std::istreambuf_iterator<char>(file)),
                                             std::istreambuf_iterator<char>());
    const auto summary = bytes_of(
        "{\"schema\":1,\"format\":\"dawproject\",\"dawproject\":{\"title\":\"Night Drive\"}}");

    auto snapshot_hash = project.value().put_blob(snapshot);
    auto samples_hash = project.value().put_blob(bytes_of("{\"entries\":[]}"));
    auto summary_hash = project.value().put_blob(summary);

    Commit::Builder builder;
    auto commit = builder.tree(TreeRef{snapshot_hash.value(), samples_hash.value()})
                      .author(identity.value().as_author())
                      .timestamp(1700000000)
                      .message("first take")
                      .auru_version("0.1.0")
                      .format_version(1)
                      .metadata(summary_hash.value())
                      .build();
    if (!commit) {
        std::cerr << "build: " << commit.error().to_string() << "\n";
        return 1;
    }

    if (auto stored = project.value().put_commit(commit.value()); !stored) {
        std::cerr << "put_commit: " << stored.error().to_string() << "\n";
        return 1;
    }
    if (auto advanced = project.value().advance_head(std::nullopt, commit.value().id());
        !advanced) {
        std::cerr << "advance_head: " << advanced.error().to_string() << "\n";
        return 1;
    }
    std::cout << "published:        " << commit.value().id().to_string() << "\n";

    // The dashboard read: the summary blob, never the snapshot.
    auto head = project.value().get_commit(project.value().head().value().value());
    auto info = project.value().project_info(head.value());
    std::cout << "ProjectInfo:      " << summary.size() << " bytes (snapshot is "
              << snapshot.size() << ")\n";
    std::cout << "title:            " << info.value()->text("title").value_or("?") << "\n";

    // Losing a compare-and-swap.
    auto conflict = project.value().advance_head(std::nullopt, commit.value().id());
    if (conflict) {
        std::cerr << "ERROR: a stale compare-and-swap was accepted\n";
        return 1;
    }
    std::cout << "CAS conflict:     provider HEAD is "
              << conflict.error().current_head.value_or("?").substr(0, 20) << "...\n";

    std::cout << "\nconsumer flow complete, linking only auru::pm\n";
    return 0;
}
