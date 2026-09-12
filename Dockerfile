# syntax=docker/dockerfile:1.7
#
# auru-pm-server, the hosted `auru-pm-v1` provider.
#
# Only the headless crates are copied in, so the rust-toolchain.toml that pins
# the GPUI desktop app to 1.95 stays out and the image builds on the oldest
# compiler the crates actually accept. That is 1.88, not the 1.86 the manifests
# still declare: the kernel uses let-chains and the lock pins a `time` that
# both need 1.88. The runtime needs nothing beyond glibc — TLS
# and the keychain are vendored — so the final stage is a slim Debian with CA
# certificates for outbound identity-provider calls and tini to reap and
# forward signals, which is what makes `docker stop` a graceful drain.

FROM rust:1.88-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock README.md ./
COPY crates/ crates/
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --locked --release -p auru-pm-server \
    && install -m 0755 target/release/auru-pm-server /usr/local/bin/auru-pm-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10002 auru-pm \
    && useradd --uid 10002 --gid 10002 --system --no-create-home \
         --home-dir /var/lib/auru-pm --shell /usr/sbin/nologin auru-pm \
    && mkdir -p /var/lib/auru-pm /etc/auru-pm \
    && chown auru-pm:auru-pm /var/lib/auru-pm
COPY --from=builder /usr/local/bin/auru-pm-server /usr/local/bin/auru-pm-server

USER 10002:10002
WORKDIR /var/lib/auru-pm
# Project state and blobs. Mount a persistent volume here.
VOLUME ["/var/lib/auru-pm"]
EXPOSE 4242
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/auru-pm-server"]
# Mount the configuration (see deploy/server.prod.example.toml) at this path,
# and the registry documents at the registry_dir it names.
CMD ["--config", "/etc/auru-pm/server.toml"]
