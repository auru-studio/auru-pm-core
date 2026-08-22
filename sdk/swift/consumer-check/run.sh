#!/usr/bin/env bash
#
# Build a separate package that depends on this one and run it against a real
# provider. Building the library proves it compiles; this proves it is usable.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"

data="$(mktemp -d)"
cleanup() {
    [ -n "${server_pid:-}" ] && kill "$server_pid" 2>/dev/null || true
    rm -rf "$data"
}
trap cleanup EXIT

cargo build --manifest-path "$repo/Cargo.toml" -p auru-pm-server --locked --quiet

port=4733
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

swift run --package-path "$here" ConsumerCheck "http://127.0.0.1:$port" "$repo"
