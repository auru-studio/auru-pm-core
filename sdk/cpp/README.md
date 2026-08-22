# auru-pm (C++)

A client for `auru-pm-v1` project-management providers.

The endpoint is always yours to supply. Anyone can run a provider, so there is
no default host here and no registry lookup — which one to trust is the user's
decision, not this library's.

```cpp
#include "auru/pm/client.hpp"

auto client = auru::pm::AuruClient::connect({
    "https://pm.example.com",
    std::make_shared<auru::pm::CurlTransport>(),
    token,
});
if (!client) { return client.error(); }

auto project = client.value().project("user/night-drive");
for (const auto& version : project.value().history({20}).value()) {
    std::cout << version.timestamp << "  " << version.message << "\n";
}
```

C++17. **No dependencies** — not even an HTTP client.

### Conan

```ini
# conanfile.txt
[requires]
auru-pm/0.1.0

[generators]
CMakeDeps
CMakeToolchain
```

```cmake
find_package(auru_pm REQUIRED)
target_link_libraries(your_app PRIVATE auru::pm)
```

The libcurl adapter is an option, off by default so that depending on this
package never drags libcurl into a build:

```sh
conan install . -o "auru-pm/*:with_curl=True"
```

### Without Conan

`find_package(auru_pm)` and `auru::pm` are the same names either way, so a plain
CMake install or `FetchContent` works without touching your build:

```cmake
find_package(auru_pm REQUIRED)
target_link_libraries(your_app PRIVATE auru::pm)
```

## What it does, and what it does not

This is a **pure C++** client. It speaks the protocol, derives commit ids, and
verifies content hashes — everything a plugin, tool, or service needs to read
and publish versions.

It does **not** read DAW project files. Normalizing an Ableton Live Set, an FL
Studio project, or a DAWproject archive, and the structured diff and merge built
on that, are twenty-odd thousand lines of format work that lives in the Rust
kernel. Reimplementing it here would mean two implementations to keep
byte-exact, and the first symptom of drift would be a provider rejecting a push.
If you need it, `auru-pm-kernel` builds as a static library with a C ABI and
links into a C++ build without ceremony.

## Bring your own HTTP

C++ has no standard HTTP client, so this library takes a `Transport` interface
rather than choosing one for you. Most applications that would use it already
have a stack, and a second one means a second TLS configuration and a second set
of certificates to keep current.

An adapter over libcurl ships alongside, off by default:

```cmake
set(AURU_PM_WITH_CURL ON)
target_link_libraries(your_app PRIVATE auru::pm auru::pm_curl)
```

Implementing your own is one method. See `consumer-check/main.cpp` for a
complete one in about sixty lines.

## Errors

Calls return `Result<T>`, not exceptions. A large part of this library's
audience — audio plugins, embedded and real-time builds — compiles with
`-fno-exceptions`, and an API they cannot use is not an API.

```cpp
auto head = project.head();
if (!head) {
    if (head.error().retryable()) { /* back off and try again */ }
    return head.error();
}
```

`error().code` is a closed enum, so a `switch` over it is exhaustive, and
`retryable()` separates a rate limit or an unreachable identity provider from a
bad token — re-prompting someone because their provider's IdP blipped would be
wrong.

If you build with exceptions, `auru/pm/throwing.hpp` buys back the verbosity:

```cpp
using auru::pm::unwrap;
auto client = unwrap(AuruClient::connect({endpoint, transport, token}));
```

## Commit identity

A commit's `id` is the BLAKE3 of the RFC 8785 canonicalization of its content.
Providers recompute it and treat a mismatch as auth-equivalent — a client
writing what it did not compute — so there is no constructor that takes an id:

```cpp
auto commit = Commit::Builder()
    .tree(TreeRef{snapshot_hash, samples_hash})
    .author(client.me().value().as_author())
    .timestamp(now)
    .message("first take")
    .auru_version("0.1.0")
    .format_version(1)
    .build();          // the id is derived here
```

Every integer on a commit must stay inside ±(2^53 − 1). RFC 8785 numbers are
IEEE-754 binary64, and beyond that bound two conformant implementations derive
different ids from the same commit — so canonical encoding fails rather than
producing bytes nobody else agrees with.

The implementation is checked against `spec/vectors/commit-encoding.json` and
`spec/vectors/content-hash.json`, the same files the TypeScript, Java and Rust
implementations are checked against.

## Reading a project cheaply

A real Live Set snapshot is around 7 MB. Every commit also stores a
`ProjectInfo` blob of a few kilobytes and points at it with `metadata`:

```cpp
auto info = project.project_info(head_commit);
if (info && info.value()) {
    render(info.value()->format, info.value()->text("title"));
}
```

`get_blob` verifies every download against its own hash. Verification is
unconditional: content addressing is only worth anything if the reader checks.
`get_blob_unverified` exists for callers verifying elsewhere, and you have to
ask for it by name.

## Losing a race

`advance_head` is a compare-and-swap. On failure the error is
`ErrorCode::HeadConflict` with `current_head` set to the provider's actual
HEAD, so you can rebase without another round trip.

## Publishing a project for the first time

A handle only exists once its profile is registered; until then every other
endpoint answers `not_found`, blob upload included:

```cpp
project.put_profile({"Night Drive", "ableton-live-set"});
```

## Threading

Copying an `AuruClient` shares its state, including the bearer token, so a
refresh on one copy is visible to the rest. A `Transport` implementation must be
safe to call from several threads if the client is shared.

## Development

```sh
cmake -S . -B build -DCMAKE_BUILD_TYPE=Debug
cmake --build build
ctest --test-dir build --output-on-failure
./consumer-check/run.sh

conan create .                          # also runs test_package
conan create . -o "&:with_curl=True"
```

The transport tests start a real `auru-pm-server` and push commits through it,
so a Rust toolchain is needed to run them. Mocked HTTP would only prove the
client agrees with the test's idea of the protocol.

`consumer-check` is the one that holds the no-dependency claim honest: it
installs the package and builds a program against it with only `auru::pm` on the
link line.

## Releasing

Tag `cpp-vX.Y.Z`. The release workflow builds and tests the package on Linux,
macOS and Windows — `conan create` runs `test_package`, which derives a commit
id from the packaged library and compares it against the published vector, so a
binary that built but encodes differently is never uploaded.

ConanCenter is deliberately not automated: submitting there is a pull request
against `conan-center-index`, reviewed by people, and it should be opened on
purpose rather than by a tag push. The workflow publishes the recipe as an
artifact so that diff is easy to prepare.
