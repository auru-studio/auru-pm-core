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
per-client request limit. It still deliberately advertises
`auth_methods: ["none"]`; don't expose it to an untrusted network.

## License

Licensed under either Apache License 2.0 or the MIT license, at your option.
