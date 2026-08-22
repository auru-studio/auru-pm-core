// swift-tools-version: 6.0

import PackageDescription

// Consumes the package the way anyone else would: as a dependency, importing
// only its public API. If something were missing from the public surface, or a
// type were not `Sendable` enough to cross an actor boundary, this is where it
// shows.
let package = Package(
    name: "ConsumerCheck",
    platforms: [.macOS(.v12)],
    dependencies: [.package(path: "..")],
    targets: [
        .executableTarget(
            name: "ConsumerCheck",
            dependencies: [
                .product(name: "AuruPM", package: "swift"),
                .product(name: "AuruPMURLSession", package: "swift"),
            ])
    ]
)
