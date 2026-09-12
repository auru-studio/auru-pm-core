# Auru PM

Auru PM keeps DAW project history in a content-addressed store. It can snapshot native Auru projects, DAWproject archives, and gzip-compressed Ableton Live Sets; commits can live on disk or move over the `auru-pm-v1` HTTP protocol.

The wire contract lives in [`spec/`](spec/) — the specification, an OpenAPI 3.1
description, JSON Schema, and frozen conformance vectors for the canonical
commit encoding. Anyone can implement a client or provider against it.

The repository is usable without Auru. The DAW remains closed source, while this project owns the file adapters, commit model, merge code, HTTP client, server, and standalone desktop client.

## Repository layout

- `crates/auru-pm-kernel` is the portable core: commit identity, project formats,
  diff, and merge. No filesystem, no sockets, no keychain — it builds for
  `wasm32-unknown-unknown`, which CI enforces, so a browser client can run it.
- `crates/auru-pm` adds the machine: the local store, discovery, providers,
  OAuth, the OS keychain, and the sync engine. It re-exports the kernel in full,
  so `auru_pm::Commit` and friends resolve unchanged.
- `crates/auru-pm-protocol` names the wire version and shared HTTP payloads.
- `crates/auru-pm-client` is the client-facing entry point. It currently re-exports the HTTP provider from the core crate so downstream code has a stable dependency before that implementation moves.
- `crates/auru-pm-server` runs the persistent reference HTTP server.
- `sdk/typescript` is `@auru/pm`, the JavaScript client. TypeScript owns the
  network; commit identity, project formats, diff, and merge come from
  `auru-pm-kernel` compiled to wasm for the browser and to an N-API addon for
  Node and Electron, so no runtime can drift from the canonical encoding.
- `crates/auru-pm-ffi` holds those bindings — one shared surface, two wrappers.
- `sdk/java` is `studio.auru:auru-pm`, a pure-Java client with no runtime
  dependencies. It implements RFC 8785 and BLAKE3 itself rather than binding to
  the kernel, so it stays a portable JAR; the published vectors are what keep it
  byte-exact. It speaks the protocol and derives commit ids, but does not read
  DAW project files.
- `sdk/cpp` is a pure C++17 client with no dependencies — not even an HTTP
  stack, which it takes as an interface so an application can plug in the one it
  already has. Distributed with Conan; `find_package(auru_pm)` and `auru::pm`
  work the same from a plain CMake install. Like the Java SDK it implements the
  canonical rule itself and does not read DAW project files.
- `sdk/swift` is a pure Swift client for Apple platforms and Linux, with no
  dependencies. `async`/`await` throughout; `AuruClient` is an actor. Transport
  is a protocol, with a `URLSession` adapter shipped separately.
- `sdk/swift-kernel` and `sdk/java/native` carry the compute kernel to mobile:
  an xcframework for iOS and macOS, `jniLibs` for Android, both built from
  `auru-pm-ffi`. They add snapshot normalization, diff and merge on device —
  the half the pure clients deliberately lack — and are optional everywhere.
- `apps/auru-pm-ui` is the GPUI desktop client. It remains a standalone nested Cargo workspace until `gpui-audio-components` has its first public revision.

Native `.auru` compatibility tests stay in the private Auru repository because they depend on its project model. Public tests use DAWproject, Ableton Live Set, and protocol fixtures.

## Build

The headless crates require Rust 1.86 or newer. The GPUI application follows
GPUI's Rust 1.95 toolchain.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

Run the reference server on port 4242:

```sh
cargo run -p auru-pm-server -- \
  --port 4242 \
  --data-dir ./auru-pm-server-data \
  --requests-per-minute 600
```

The server persists project state and content-addressed blobs and applies a
per-client request limit. With no `--config`, it retains the development
default: no authentication and a loopback-only listener.

## HTTPS for a local device

Android has refused cleartext HTTP by default since API 28, so a phone or
emulator cannot reach a plain-HTTP development server at all. Add `--tls`:

```sh
cargo run -p auru-pm-server -- --tls
```

The first run mints a self-signed authority at
`$XDG_DATA_HOME/auru-pm/development-tls/development-ca.pem` (usually under
`~/.local/share`) and keeps it; every run issues a fresh leaf from it. The
authority lives per user rather than per `data_dir` on purpose: a client bakes
the anchor in at build time, so an authority per data directory would break an
already-installed client the moment `--data-dir` changed.

A loopback listener is only reachable from the host. To reach it from a device:

```sh
cargo run -p auru-pm-server -- --tls --listen 0.0.0.0:4242 --allow-insecure-non-loopback
```

The flag is required because an unauthenticated listener off loopback should
never happen by accident. Startup then prints the addresses to paste:

```
TLS: self-signed development certificate for localhost, 127.0.0.1, ::1, 10.0.2.2, 192.168.1.122
     trust anchor: /home/you/.local/share/auru-pm/development-tls/development-ca.pem
     fingerprint:  67:85:CF:AB:…:A5:89
device access: https://192.168.1.122:4242
```

The fingerprint is what a client bundles. `:app:generateDebugDevelopmentTrustAnchor`
in `auru-pm-mobile` prints the same value for the anchor it built into the APK;
if the two differ, that build trusts an authority this server does not use, and
the symptom is a handshake failure naming neither certificate. Rebuild the
client.

The authority is never replaced silently. If one of its two files goes missing
the server refuses to start rather than minting a new one, because minting one
invalidates every client that already bundled the old anchor. Delete both files
deliberately to start over, then rebuild the clients.

Every non-loopback interface address goes into the certificate, so the printed
URL works as-is. Add more names with `--tls-san studio.local`, repeated as
needed.

**`10.0.2.2` is not reliable.** The Android emulator's alias for the host's
loopback address is in the certificate, and it works from some system images,
but on others an app's traffic to it is silently dropped while `adb shell`
reaches it fine — a fifteen-second connect timeout with no other symptom. Use
the LAN address the banner prints; it behaves the same on an emulator and on a
physical device.

To serve a chain issued elsewhere — mkcert, an internal CA, a staging
certificate — pass `--tls-cert fullchain.pem --tls-key privkey.pem`, or set
`[tls] mode = "files"`. A deployment should do neither: the reverse proxy in
front owns TLS, and the server listens on private HTTP behind it.

## Standards-based authentication

For a deployable server, copy
[`server.example.toml`](crates/auru-pm-server/server.example.toml), register its
fixed loopback redirect URI as a public/native OAuth client with your identity
provider, and run:

```sh
cargo run -p auru-pm-server -- --config ./server.toml
```

The provider must publish OAuth 2.0 Authorization Server Metadata (RFC 8414) or
OpenID Connect discovery. The desktop uses Authorization Code with PKCE S256;
device authorization is also supported when both the config and discovery
document advertise it. The server validates tokens using exactly one
configured strategy:

- `strategy = "jwt"` discovers and caches the provider's JWKS, refreshing once
  when key rotation presents an unknown `kid`.
- `strategy = "introspection"` validates opaque tokens with RFC 7662. Add
  `endpoint = "https://..."` only when discovery does not publish one, plus
  `client_id` and `client_secret_env`. Put the secret in that named environment
  variable, never in TOML.

Issuer, audience, subject, expiry/activity, and `required_scope` are enforced.
`openid` is the default scope so providers such as Clerk that do not generally
offer custom OAuth scopes can still be configured without a vendor adapter.
The external reverse proxy owns TLS; `public_base_url` and all identity-provider
endpoints must use HTTPS, while the server itself listens on private HTTP.

Projects are private to the identity key `(issuer, subject)`. Existing data
from an older unauthenticated server is not assigned implicitly: OAuth startup
refuses until `legacy_owner_subject` explicitly names its owner. Access and
refresh tokens are stored only in the desktop OS keychain.

### Deploying the server

[`deploy/`](deploy/) holds what a hosted instance needs beyond the binary:
[`server.prod.example.toml`](deploy/server.prod.example.toml) is the
configuration the root [`Dockerfile`](Dockerfile) expects at
`/etc/auru-pm/server.toml`, and [`deploy/registry/`](deploy/registry/) holds
the `providers.json` and `plugins.json` documents the server publishes at
`/providers.json` and `/plugins.json` when `registry_dir` points at them.
`plugins.json` there is a verbatim copy of the bundled
`crates/auru-pm/data/plugins.json`; CI refuses to build the image when they
differ, because the app replaces its bundled list with the served one.
`release-server.yml` publishes the image to
`ghcr.io/<owner>/auru-pm-server`, tagged by commit and by `server-v*` tag, and
records the digest to pin.

At runtime the server logs to stderr through `tracing` — `RUST_LOG` filters
it, `AURU_PM_LOG_FORMAT=json` switches to JSON lines — answers `GET /v1/ready`
outside authentication and the rate limiter for an orchestrator's readiness
probe, waits for the identity provider with a backoff at startup rather than
exiting, and drains open connections on SIGTERM.

## GPUI inspection and automation

The desktop app integrates the published `gpui-mcp` crate behind an explicit
development/test flag. Start the app with inspection enabled:

```sh
cargo run --manifest-path apps/auru-pm-ui/Cargo.toml -- --inspect
```

The app prints an ephemeral loopback address and authentication token. Install
the matching MCP server with `cargo install gpui-mcp`, configure the
`gpui-mcp` command in your MCP client, then call its `attach` tool with those
two values.

The semantic tree exposes the real library, projects, onboarding, settings,
provider picker, and authentication state. Click and focus actions are
dispatched on GPUI's foreground executor; keyboard input, Unicode text,
scrolling, dragging, and resizing use GPUI's normal event path. Inspection is
off for every ordinary launch, and each inspected launch receives a new token.
Runtime screenshot capture is not available on GPUI's current Linux backend;
the MCP screenshot tool returns an explicit error there instead of writing a
misleading artifact.

## License

Licensed under either Apache License 2.0 or the MIT license, at your option.
