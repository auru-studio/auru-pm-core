// swift-tools-version: 6.0

import PackageDescription

// A separate package on purpose. A binary target makes a package unbuildable on
// Linux, and `AuruPM` has to stay source-only so a server or a CI job can use
// it. Depend on this one only where the compute half is actually wanted —
// a phone showing a diff, not a backend listing history.
let package = Package(
    name: "AuruPMKernel",
    platforms: [.macOS(.v12), .iOS(.v15), .tvOS(.v15), .visionOS(.v1)],
    products: [
        .library(name: "AuruPMKernel", targets: ["AuruPMKernel"])
    ],
    dependencies: [.package(path: "../swift")],
    targets: [
        .binaryTarget(
            name: "AuruPMKernelFFI",
            path: "Artifacts/AuruPMKernelFFI.xcframework"),
        .target(
            name: "AuruPMKernel",
            dependencies: [
                .product(name: "AuruPM", package: "swift"),
                "AuruPMKernelFFI",
            ]),
        .testTarget(
            name: "AuruPMKernelTests",
            dependencies: [
                "AuruPMKernel",
                .product(name: "AuruPM", package: "swift"),
            ]),
    ],
    swiftLanguageModes: [.v5]
)
