// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "AuruPM",
    platforms: [
        .macOS(.v12), .iOS(.v15), .tvOS(.v15), .watchOS(.v8), .visionOS(.v1),
    ],
    products: [
        // The core has no dependencies and no HTTP stack: transport is a
        // protocol the caller supplies, because an application that would use
        // this generally already made that choice.
        .library(name: "AuruPM", targets: ["AuruPM"]),
        // The adapter is a separate product so linking the core never pulls in
        // URLSession — which matters on Linux, where it lives in a different
        // module entirely.
        .library(name: "AuruPMURLSession", targets: ["AuruPMURLSession"]),
    ],
    targets: [
        .target(name: "AuruPM"),
        .target(name: "AuruPMURLSession", dependencies: ["AuruPM"]),
        .testTarget(name: "AuruPMTests", dependencies: ["AuruPM", "AuruPMURLSession"]),
    ],
    swiftLanguageModes: [.v5]
)
