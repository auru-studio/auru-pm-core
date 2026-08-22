# auru-pm (Java)

A client for `auru-pm-v1` project-management providers.

The endpoint is always yours to supply. Anyone can run a provider, so there is
no default host here and no registry lookup — which one to trust is the user's
decision, not this library's.

```java
AuruClient client = AuruClient.to("https://pm.example.com")
    .accessToken(token)
    .connect();

ProjectClient project = client.project("user/night-drive");
for (CommitSummary version : project.history(20)) {
    System.out.println(Instant.ofEpochSecond(version.timestamp()) + "  " + version.message());
}
```

```kotlin
dependencies {
    implementation("studio.auru:auru-pm:0.1.0")
}
```

Java 17 or newer. **No runtime dependencies** — a client library that drags in a
JSON stack causes version conflicts in somebody else's application, and
canonical encoding needs exact control over JSON output, so a third-party
serializer's formatting choices would become this library's bug.

## What it does, and what it does not

This is a **pure Java** client. It speaks the protocol, derives commit ids, and
verifies content hashes — everything a backend service, catalogue, or automation
pipeline needs.

It does **not** read DAW project files. Normalizing an Ableton Live Set, an FL
Studio project, or a DAWproject archive, and the structured diff and merge built
on that, are twenty-odd thousand lines of format work that lives in the Rust
kernel. Reimplementing it here would mean two implementations to keep byte-exact
with each other, and the first symptom of drift would be a provider rejecting a
push.

That boundary is deliberate: commit identity is the part providers verify, and
it does not need any of that machinery.

## Commit identity

A commit's `id` is the BLAKE3 of the RFC 8785 canonicalization of its content.
Providers recompute it and treat a mismatch as auth-equivalent — a client
writing what it did not compute — so there is no constructor that takes an id:

```java
Commit commit = Commit.builder()
    .tree(new TreeRef(snapshotHash, samplesHash))
    .author(client.me().asAuthor())
    .timestamp(Instant.now().getEpochSecond())
    .message("first take")
    .auruVersion("0.1.0")
    .formatVersion(1)
    .build();          // the id is derived here

project.putCommit(commit);
```

`Commit.fromJson` keeps the id a provider sent; `verifyId()` checks it.

Every integer on a commit must stay inside ±(2^53 − 1). RFC 8785 numbers are
IEEE-754 binary64, and beyond that bound two conformant implementations derive
different ids from the same commit — so canonical encoding throws rather than
producing bytes nobody else agrees with.

The implementation is checked against `spec/vectors/commit-encoding.json` and
`spec/vectors/content-hash.json`, the same files the TypeScript SDK and the Rust
implementation are checked against.

## Reading a project cheaply

A real Live Set snapshot is around 7 MB. Every commit also stores a
`ProjectInfo` blob of a few kilobytes — tempo, key, tracks, plugins — and points
at it with `metadata`. A list view should read that:

```java
Commit head = project.getCommit(project.head().orElseThrow());
project.projectInfo(head).ifPresent(info -> render(info.format(), info.field("title")));
```

`getBlob` verifies every download against its own hash and throws if the
provider returned different bytes. Verification is unconditional: content
addressing is only worth anything if the reader checks.

## Losing a race

`advanceHead` is a compare-and-swap. When someone else got there first it throws
`HeadConflictException`, carrying the provider's actual HEAD so you can rebase
without another round trip:

```java
try {
    project.advanceHead(Optional.of(knownHead), commit.id());
} catch (HeadConflictException conflict) {
    rebaseOnto(conflict.current());
}
```

Every provider error is an `AuruException` with a `code` from a closed enum, so
a `switch` over it is exhaustive. `retryable()` separates a rate limit or an
unreachable identity provider from a bad token — re-prompting someone to sign in
because their provider's IdP blipped would be wrong.

## Signing in

Authorization Code with PKCE, for providers that advertise it. Endpoints come
from the issuer's own discovery document, never from the provider's health
response — a provider that could name its own token endpoint could name someone
else's — and a document whose `issuer` does not match exactly is refused.

```java
OAuthConfiguration configuration = client.health().authentication().orElseThrow();
OAuthConfiguration.OAuthClient app =
    configuration.client(OAuthConfiguration.Kind.NATIVE).orElseThrow();

OAuth.ServerMetadata metadata = OAuth.discover(configuration.issuer());
OAuth.AuthorizationRequest request =
    OAuth.beginAuthorization(metadata, app, configuration.requiredScope());

openInBrowser(request.url());                  // then catch the redirect
String code = request.codeFrom(redirectUrl);   // checks `state` for you

OAuth.AccessToken token = OAuth.completeAuthorization(metadata, app, request, code);
client.accessToken(token.token());
```

`completeAuthorization` **discards the refresh token**. A caller who is handed
one will eventually store it, and most places it could be stored are worse than
not having it at all. When the access token expires, run the flow again.

`completeAuthorizationWithRefresh` is the explicit opt-in for callers that have
a real secret store — an OS keychain, a secrets manager. A refresh token is a
long-lived credential for the whole account; treat it the way you would treat a
password, and never let one reach a log or a configuration dump.

`beginAuthorization` refuses a provider that does not advertise `S256`. A plain
challenge is not a fallback worth having: the point of PKCE is that an
intercepted authorization code is useless, which a plain challenge does not
give you.

## Publishing a project for the first time

A handle only exists once its profile is registered; until then every other
endpoint answers `not_found`, blob upload included:

```java
project.putProfile(new ProjectProfile("Night Drive", "ableton-live-set"));
```

## The compute kernel (optional, Android)

`AuruClient` cannot read a DAW project file — that is twenty-odd thousand lines
of format work in the Rust kernel, and a second implementation would drift from
the first. `Kernel` links that work as a native library so a phone can answer
"what changed between these two versions":

```java
Kernel.loadFromSystemLibraryPath();   // jniLibs/<abi>/libauru_pm_ffi.so

byte[] snapshot = Kernel.snapshotFromSource("ableton-live-set", fileBytes);
Optional<ProjectInfo> info = Kernel.projectInfo(snapshot);
List<String> changes = Kernel.summarize(previous, snapshot);
Json diff = Kernel.diff(previous, snapshot);
```

Optional in the strict sense: nothing else in this library references `Kernel`,
so an application that never calls it never loads it and never needs the native
library — the same arrangement that keeps `JdkHttpTransport` out of an Android
build.

Build the libraries with `sdk/java-kernel-build.sh`, which produces
`jniLibs/arm64-v8a` and `jniLibs/x86_64` (device and emulator) at roughly 2.3 MB
each. Point an Android module's `sourceSets` at that directory, or copy it into
`src/main/jniLibs`.

`Kernel.commitId` and `Commit`'s own derivation agree case by case against the
published vectors — the test suite asserts it, because if they ever did not, a
phone with the kernel and a phone without would publish commits the other could
not verify. `Kernel.matchesClientProtocol()` is worth asserting at startup.

## Threading

An `AuruClient` is safe to share. The only mutable state is the bearer token,
held in memory and written nowhere else; persisting one is the caller's decision
and should use a real secret store rather than a file beside the application.

Calls are blocking. The JDK's `HttpClient` makes an async layer easy to add
later without breaking anything; the reverse is not true.

## Development

```sh
./gradlew build            # compile, test, javadoc
./consumer-check/run.sh    # use the jar with nothing else on the classpath
./gradlew verifyPublishable
```

The transport tests start a real `auru-pm-server` and push commits through it,
so a Rust toolchain is needed to run them. Mocked HTTP would only prove the
client agrees with the test's idea of the protocol.

`consumer-check` is the one that holds the zero-dependency claim honest: it
compiles a program against the jar and runs it with only that jar on the
classpath. `verifyPublishable` checks the artifact set and the POM against what
Maven Central insists on, so a missing `<developers>` block fails in seconds
rather than after a version has been tagged.

## Releasing

Tag `java-vX.Y.Z`. The release workflow runs the full suite and the consumer
check, signs the artifacts, and uploads a bundle to the Central Portal as
`USER_MANAGED` — the deployment lands validated and a person publishes it, so a
mistake caught at that point costs a click instead of a permanent version.
