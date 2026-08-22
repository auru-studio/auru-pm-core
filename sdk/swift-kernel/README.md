# AuruPMKernel

The compute half, as a native library for Apple platforms.

[`AuruPM`](../swift) on its own speaks the protocol, derives commit ids, and
verifies hashes — everything needed to browse history and publish. It cannot
read a DAW project file, because that is twenty-odd thousand lines of format
work and a second implementation would drift from the first.

This package links that work rather than reimplementing it, so a phone can
answer "what changed between these two versions" with the same code the desktop
runs.

```swift
import AuruPM
import AuruPMKernel

let snapshot = try Kernel.snapshot(fromSource: fileBytes, format: "ableton-live-set")
let info = try Kernel.projectInfo(fromSnapshot: snapshot)

let changes = try Kernel.summarize(before: previous, after: snapshot)
let diff = try Kernel.diff(before: previous, after: snapshot)
```

```swift
.package(url: "https://github.com/auru-studio/auru-pm-core.git", from: "0.1.0")
// then depend on the AuruPMKernel product
```

## Why this is a separate package

A binary target makes a Swift package unbuildable on Linux, and `AuruPM` has to
stay source-only so a server or a CI job can use it. Depend on this one only
where the compute half is actually wanted — a phone showing a diff, not a
backend listing history.

## Slices

arm64 only, deliberately: every shipping iPhone and iPad is arm64, and every
Apple Silicon Mac runs the arm64 simulator.

| Slice | For |
| --- | --- |
| `ios-arm64` | devices |
| `ios-arm64-simulator` | the simulator on Apple Silicon |
| `macos-arm64` | a desktop app, and `swift test` |

macOS is included so the kernel can be exercised with plain `swift test`.
Without a host slice it could only ever run in a simulator, which is a poor
place to find out a hash is wrong.

Add `x86_64-apple-ios` and `x86_64-apple-darwin` in
`scripts/build-xcframework.sh` if an Intel Mac has to build or run this.

## Sizes

The static archive is large; only the linked contribution matters, and the
linker takes what is used.

| Artifact | Size |
| --- | --- |
| xcframework (all slices, unstripped static archives) | ~63 MB |
| linked shared library, one architecture | ~1.9 MB |

## Both implementations must agree

`Kernel.commitID` and `AuruPM`'s own `Commit` derive the same id from the same
commit — two implementations of one specification, checked against the same
vectors. The test suite asserts they agree case by case, because if they ever
did not, a phone with the kernel and a phone without would publish commits the
other could not verify.

`Kernel.matchesClientProtocol` is worth asserting at startup: a binary artifact
and a source package can drift out of step in a way neither notices otherwise.

## Building

```sh
./scripts/build-xcframework.sh
swift test
```

Needs a Rust toolchain with the `aarch64-apple-ios`, `aarch64-apple-ios-sim` and
`aarch64-apple-darwin` targets, and Xcode for `xcodebuild -create-xcframework`.
