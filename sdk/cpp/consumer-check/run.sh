#!/usr/bin/env bash
#
# Install the library, then build and run a consumer against the installed
# package with only `auru::pm` on the link line. The library claims no
# dependencies, and this is the check that actually holds it to that.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdk="$(dirname "$here")"
repo="$(cd "$sdk/../.." && pwd)"

prefix="$(mktemp -d)"
build="$(mktemp -d)"
data="$(mktemp -d)"
trap 'rm -rf "$prefix" "$build" "$data"; [ -n "${server_pid:-}" ] && kill "$server_pid" 2>/dev/null || true' EXIT

cmake -S "$sdk" -B "$build/lib" -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DAURU_PM_BUILD_TESTS=OFF -DCMAKE_BUILD_TYPE=Release > /dev/null
cmake --build "$build/lib" --target install > /dev/null

cmake -S "$here" -B "$build/consumer" -DCMAKE_PREFIX_PATH="$prefix" > /dev/null
cmake --build "$build/consumer" > /dev/null

cargo build --manifest-path "$repo/Cargo.toml" -p auru-pm-server --locked --quiet

port=4711
cat > "$data/server.toml" <<TOML
version = 1
provider_id = "consumer-check"
listen = "127.0.0.1:$port"
data_dir = "$data/data"
requests_per_minute = 100000

[authentication]
mode = "none"
TOML

"$repo/target/debug/auru-pm-server" --config "$data/server.toml" > /dev/null 2>&1 &
server_pid=$!

for _ in $(seq 1 100); do
    if curl -fsS "http://127.0.0.1:$port/v1/health" > /dev/null 2>&1; then break; fi
    sleep 0.1
done

"$build/consumer/consumer_check" "http://127.0.0.1:$port" "$repo"
