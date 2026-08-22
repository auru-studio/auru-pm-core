# AuruPM

A client for `auru-pm-v1` project-management providers, for Apple platforms and
Linux.

The endpoint is always yours to supply. Anyone can run a provider, so there is
no default host here and no registry lookup — which one to trust is the user's
decision, not this library's.

```swift
import AuruPM
import AuruPMURLSession

let client = try await AuruClient.connect(
    endpoint: "https://pm.example.com",
    transport: URLSessionTransport(),
    accessToken: token)

for version in try await client.project("user/night-drive").history(limit: 20) {
    print(Date(timeIntervalSince1970: TimeInterval(version.timestamp)), version.message)
}
```

```swift
.package(url: "https://github.com/auru-studio/auru-pm-core.git", from: "0.1.0")
```

Swift 6 tools, language mode 5. **No dependencies.**

## What it does, and what it does not

This is a **pure Swift** client. It speaks the protocol, derives commit ids, and
verifies content hashes — everything a mobile or desktop client needs to browse
history, read what a version is, and publish.

It does **not** read DAW project files. Normalizing an Ableton Live Set, an FL
Studio project, or a DAWproject archive, and the structured diff and merge built
on that, are twenty-odd thousand lines of format work that lives in the Rust
kernel. Reimplementing it here would mean two implementations to keep
byte-exact, and the first symptom of drift would be a provider rejecting a push.

**On a phone this matters less than it sounds.** Plugin availability is
irrelevant to everything above: `ProjectInfo` carries tempo, key, tracks *and
plugin references by name*, so a device with none of the plugins installed can
still show what a version is and what it would need. What a phone cannot do
without the kernel is answer "what changed between these two versions".

## Two halves

**Transport is yours.** `URLSession` is the obvious choice on Apple platforms
and ships as `AuruPMURLSession`, but on Linux it lives in a different module,
and an application with its own networking layer — its own retry policy, its own
certificate pinning, its own instrumentation — should not be made to run a
second one. Conform to `Transport`; it is one method.

**Everything else is pure Swift**, including BLAKE3 and SHA-256. CryptoKit does
not exist on Linux, and swift-crypto would be a dependency for one hash in a
package that otherwise has none.

## Commit identity

A commit's `id` is the BLAKE3 of the RFC 8785 canonicalization of its content.
Providers recompute it and treat a mismatch as auth-equivalent — a client
writing what it did not compute — so no initializer accepts an id:

```swift
let commit = try Commit(
    tree: TreeRef(snapshot: snapshotHash, samples: samplesHash),
    author: try await client.me().asAuthor,
    timestamp: Int64(Date().timeIntervalSince1970),
    message: "first take",
    auruVersion: "0.1.0",
    formatVersion: 1)      // the id is derived here

try await project.putCommit(commit)
```

`Commit(json:)` keeps the id a provider sent; `verifyID()` checks it.

Every integer on a commit must stay inside ±(2^53 − 1). RFC 8785 numbers are
IEEE-754 binary64, and beyond that bound two conformant implementations derive
different ids from the same commit — so canonical encoding throws rather than
producing bytes nobody else agrees with.

Checked against `spec/vectors/commit-encoding.json` and
`spec/vectors/content-hash.json`, the same files the Rust, TypeScript, Java and
C++ implementations are checked against.

## Reading a project cheaply

A real Live Set snapshot is around 7 MB. Every commit also stores a
`ProjectInfo` blob of a few kilobytes and points at it with `metadata`:

```swift
if let info = try await project.projectInfo(for: head) {
    render(info.format, info.text("title"), info.number("tempo"))
}
```

`getBlob` verifies every download against its own hash. Verification is
unconditional: content addressing is only worth anything if the reader checks.
`getBlobUnverified` exists for callers verifying elsewhere, and you have to ask
for it by name.

## Losing a race

`advanceHead` is a compare-and-swap. On failure the error is `.headConflict`
with `currentHead` set to the provider's actual HEAD, so you can rebase without
another round trip:

```swift
do {
    try await project.advanceHead(from: knownHead, to: commit.id)
} catch let error as AuruError where error.code == .headConflict {
    try await rebase(onto: error.currentHead)
}
```

`AuruError.code` is a closed enum, so a `switch` over it is exhaustive, and
`isRetryable` separates a rate limit or an unreachable identity provider from a
bad token — re-prompting someone because their provider's IdP blipped would be
wrong.

## Tokens

`AuruClient` is an actor, so the bearer token is safe to refresh from anywhere.
It lives in memory and nowhere else; persisting one is your decision and belongs
in the Keychain.

`OAuth.completeAuthorization` **discards the refresh token**. A caller who is
handed one will eventually store it, and most places it could be stored are
worse than not having it at all.
`OAuth.completeAuthorizationWithRefresh` is the explicit opt-in for callers
using the Keychain.

Endpoints come from the issuer's own discovery document, never from the
provider's health response, and a document whose `issuer` does not match exactly
is refused.

## Publishing a project for the first time

A handle only exists once its profile is registered; until then every other
endpoint answers `notFound`, blob upload included:

```swift
try await project.putProfile(ProjectProfile(displayName: "Night Drive", format: "ableton-live-set"))
```

## Development

```sh
swift test
./consumer-check/run.sh
```

The transport tests start a real `auru-pm-server` and push commits through it,
so a Rust toolchain is needed to run them. Mocked HTTP would only prove the
client agrees with the test's idea of the protocol.

`consumer-check` builds a separate package that depends on this one and runs a
real flow through its public API.
