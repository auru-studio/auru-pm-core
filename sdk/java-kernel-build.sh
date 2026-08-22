#!/usr/bin/env bash
#
# Build the compute kernel as an Android shared library.
#
# The kernel is the half the pure Java client deliberately lacks: snapshot
# normalization, structured diff, and merge. It ships as `jniLibs`, separate
# from the jar, so a backend that only needs the protocol never carries it.
#
# Set ANDROID_NDK_HOME if the NDK is not where Homebrew puts it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

ndk="${ANDROID_NDK_HOME:-/opt/homebrew/share/android-ndk}"
if [ ! -d "$ndk/toolchains/llvm/prebuilt" ]; then
    echo "no Android NDK at $ndk; set ANDROID_NDK_HOME" >&2
    exit 1
fi
toolchain="$(echo "$ndk"/toolchains/llvm/prebuilt/*/bin)"

# API 21 is the floor the NDK still supports and covers every device that can
# run a modern app. It bounds the native library only; the jar's own minimum is
# a separate question.
api=21

# arm64 first because every shipping Android device is arm64. x86_64 is here
# for the emulator, which is where a POC is usually run.
declare -a targets=(
    "aarch64-linux-android:arm64-v8a:aarch64-linux-android${api}-clang"
    "x86_64-linux-android:x86_64:x86_64-linux-android${api}-clang"
)

out="$here/java/native/jniLibs"
rm -rf "$out"

for entry in "${targets[@]}"; do
    IFS=":" read -r target abi linker <<< "$entry"

    if ! rustup target list --installed | grep -q "^${target}$"; then
        echo "skipping $abi: rustup target $target is not installed" >&2
        continue
    fi

    echo "building $abi ($target)"
    # Modern NDKs ship one `llvm-ar` rather than a per-target one, and cc-rs
    # still looks for the old name unless told otherwise.
    env "CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_LINKER=$toolchain/$linker" \
        "CC_${target}=$toolchain/$linker" \
        "AR_${target}=$toolchain/llvm-ar" \
        "RANLIB_${target}=$toolchain/llvm-ranlib" \
        cargo build --manifest-path "$repo/Cargo.toml" \
            -p auru-pm-ffi --features jni --release --target "$target" --locked

    mkdir -p "$out/$abi"
    cp "$repo/target/$target/release/libauru_pm_ffi.so" "$out/$abi/"
    ls -la "$out/$abi/libauru_pm_ffi.so" | awk -v abi="$abi" '{printf "  %s: %.1f MB\n", abi, $5/1048576}'
done

echo
echo "jniLibs: $out"
echo "Copy into an Android module's src/main/jniLibs, or point sourceSets at it."
