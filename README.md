# Auru PM

Auru PM keeps DAW project history in a content-addressed store. It can snapshot native Auru projects, DAWproject archives, and gzip-compressed Ableton Live Sets; commits can live on disk or move over the `auru-pm-v1` HTTP protocol.

The repository is usable without Auru. The DAW remains closed source, while this project owns the file adapters, commit model, merge code, HTTP client, server, and standalone desktop client.

## Repository layout

- `crates/auru-pm` contains snapshots, commits, diffs, merges, the local store, and provider traits.
- `crates/auru-pm-protocol` names the wire version and shared HTTP payloads.
- `crates/auru-pm-client` is the client-facing entry point. It currently re-exports the HTTP provider from the core crate so downstream code has a stable dependency before that implementation moves.
- `crates/auru-pm-server` runs the persistent reference HTTP server.
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

## License

Licensed under either Apache License 2.0 or the MIT license, at your option.
