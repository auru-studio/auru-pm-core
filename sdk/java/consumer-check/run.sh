#!/usr/bin/env bash
#
# Build the jar and use it the way a consumer would: nothing on the classpath
# but auru-pm.jar itself. The library claims zero runtime dependencies, and this
# is the only check that actually holds it to that.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sdk="$(dirname "$here")"
repo="$(cd "$sdk/../.." && pwd)"

"$sdk/gradlew" -p "$sdk" --no-daemon --quiet jar
cargo build --manifest-path "$repo/Cargo.toml" -p auru-pm-server --locked --quiet

jar="$(ls "$sdk"/build/libs/auru-pm-*.jar | grep -v -- '-sources\|-javadoc' | head -1)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

javac -cp "$jar" -d "$out" "$here/ConsumerCheck.java"
java -cp "$jar:$out" ConsumerCheck "$repo"
