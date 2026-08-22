#pragma once

#include <netdb.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "auru/pm/transport.hpp"

/// A minimal HTTP/1.1 transport over a plain socket, for the tests.
///
/// Deliberately small and plaintext-only: it exists to exercise the real client
/// against a real provider on loopback, and to stand as a worked example of what
/// implementing `Transport` involves. Nothing here belongs in production — no
/// TLS, no redirects, no connection reuse, no proxy support. That is the point
/// of the interface: an application brings its own stack.
class SocketTransport final : public auru::pm::Transport {
public:
    auru::pm::Result<auru::pm::Response> send(const auru::pm::Request& request) override {
        std::string host;
        std::string port = "80";
        std::string path;
        if (!split(request.url, host, port, path)) {
            return auru::pm::make_error<auru::pm::Response>(auru::pm::ErrorCode::BadRequest,
                                                            "cannot parse " + request.url);
        }

        const int socket_fd = connect_to(host, port);
        if (socket_fd < 0) {
            return auru::pm::make_error<auru::pm::Response>(
                auru::pm::ErrorCode::Internal, "cannot reach " + host + ":" + port);
        }

        std::string wire = request.method + " " + path + " HTTP/1.1\r\nHost: " + host
                           + "\r\nConnection: close\r\n";
        for (const auto& header : request.headers) {
            wire += header.first + ": " + header.second + "\r\n";
        }
        wire += "Content-Length: " + std::to_string(request.body.size()) + "\r\n\r\n";
        wire.append(reinterpret_cast<const char*>(request.body.data()), request.body.size());

        if (!write_all(socket_fd, wire)) {
            ::close(socket_fd);
            return auru::pm::make_error<auru::pm::Response>(auru::pm::ErrorCode::Internal,
                                                            "the connection closed while writing");
        }

        std::string raw;
        char buffer[8192];
        ssize_t read_bytes = 0;
        while ((read_bytes = ::read(socket_fd, buffer, sizeof(buffer))) > 0) {
            raw.append(buffer, static_cast<std::size_t>(read_bytes));
        }
        ::close(socket_fd);

        return parse(raw);
    }

private:
    static bool split(const std::string& url, std::string& host, std::string& port,
                      std::string& path) {
        const auto scheme_end = url.find("://");
        if (scheme_end == std::string::npos) {
            return false;
        }
        const auto authority_start = scheme_end + 3;
        const auto path_start = url.find('/', authority_start);
        const std::string authority = url.substr(
            authority_start,
            path_start == std::string::npos ? std::string::npos : path_start - authority_start);
        path = path_start == std::string::npos ? "/" : url.substr(path_start);

        const auto colon = authority.rfind(':');
        if (colon == std::string::npos) {
            host = authority;
        } else {
            host = authority.substr(0, colon);
            port = authority.substr(colon + 1);
        }
        return !host.empty();
    }

    static int connect_to(const std::string& host, const std::string& port) {
        addrinfo hints{};
        hints.ai_family = AF_UNSPEC;
        hints.ai_socktype = SOCK_STREAM;

        addrinfo* candidates = nullptr;
        if (::getaddrinfo(host.c_str(), port.c_str(), &hints, &candidates) != 0) {
            return -1;
        }

        int socket_fd = -1;
        for (addrinfo* candidate = candidates; candidate != nullptr;
             candidate = candidate->ai_next) {
            socket_fd = ::socket(candidate->ai_family, candidate->ai_socktype,
                                 candidate->ai_protocol);
            if (socket_fd < 0) {
                continue;
            }
            if (::connect(socket_fd, candidate->ai_addr, candidate->ai_addrlen) == 0) {
                break;
            }
            ::close(socket_fd);
            socket_fd = -1;
        }
        ::freeaddrinfo(candidates);
        return socket_fd;
    }

    static bool write_all(int socket_fd, const std::string& data) {
        std::size_t sent = 0;
        while (sent < data.size()) {
            const ssize_t written = ::write(socket_fd, data.data() + sent, data.size() - sent);
            if (written <= 0) {
                return false;
            }
            sent += static_cast<std::size_t>(written);
        }
        return true;
    }

    static auru::pm::Result<auru::pm::Response> parse(const std::string& raw) {
        const auto header_end = raw.find("\r\n\r\n");
        if (header_end == std::string::npos) {
            return auru::pm::make_error<auru::pm::Response>(auru::pm::ErrorCode::Internal,
                                                            "truncated HTTP response");
        }

        auru::pm::Response response;
        const std::string head = raw.substr(0, header_end);

        std::size_t line_start = head.find("\r\n");
        const std::string status_line = head.substr(0, line_start);
        const auto first_space = status_line.find(' ');
        response.status = std::atoi(status_line.substr(first_space + 1, 3).c_str());

        while (line_start != std::string::npos) {
            const std::size_t next = head.find("\r\n", line_start + 2);
            const std::string line = head.substr(
                line_start + 2,
                next == std::string::npos ? std::string::npos : next - line_start - 2);
            const auto colon = line.find(':');
            if (colon != std::string::npos) {
                std::string value = line.substr(colon + 1);
                while (!value.empty() && value.front() == ' ') {
                    value.erase(value.begin());
                }
                response.headers.emplace_back(line.substr(0, colon), value);
            }
            line_start = next;
        }

        const std::string body = raw.substr(header_end + 4);
        response.body.assign(body.begin(), body.end());
        return auru::pm::Result<auru::pm::Response>::ok(std::move(response));
    }
};
