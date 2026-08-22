#!/usr/bin/env bash
#
# Build the compute kernel as an xcframework for Apple platforms.
#
# The kernel is the half the pure Swift client deliberately lacks: snapshot
# normalization, structured diff, and merge. It ships as a separate package so
# `AuruPM` itself stays source-only — a binary target makes a package
# unbuildable on Linux, and a dashboard backend should not pay for a phone's
# feature.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
package="$(dirname "$here")"
repo="$(cd "$package/../.." && pwd)"

# arm64 only, deliberately. Every shipping iPhone and iPad is arm64, and every
# Apple Silicon Mac runs the arm64 simulator. Add the x86_64 variants only if an
# Intel Mac has to build or run this.
#
# macOS is included so the package builds and its tests run with plain
# `swift test` — without a host slice the kernel could only ever be exercised
# through a simulator, which is a poor place to find out a hash is wrong.
targets=(aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin)

for target in "${targets[@]}"; do
    echo "building $target"
    cargo build --manifest-path "$repo/Cargo.toml" \
        -p auru-pm-ffi --features capi --release --target "$target" --locked
done

headers="$(mktemp -d)"
trap 'rm -rf "$headers"' EXIT
cp "$repo/crates/auru-pm-ffi/include/auru_pm.h" "$headers/"
cat > "$headers/module.modulemap" <<'MODULEMAP'
module AuruPMKernelFFI {
    header "auru_pm.h"
    export *
}
MODULEMAP

output="$package/Artifacts/AuruPMKernelFFI.xcframework"
rm -rf "$output"
mkdir -p "$package/Artifacts"

xcodebuild -create-xcframework \
    -library "$repo/target/aarch64-apple-ios/release/libauru_pm_ffi.a" -headers "$headers" \
    -library "$repo/target/aarch64-apple-ios-sim/release/libauru_pm_ffi.a" -headers "$headers" \
    -library "$repo/target/aarch64-apple-darwin/release/libauru_pm_ffi.a" -headers "$headers" \
    -output "$output" > /dev/null

echo "xcframework: $output"
du -sh "$output" | awk '{print "  archive size: " $1 " (static; the linker takes only what is used)"}'
