import Foundation

/// Where the repository lives, derived from this file's own path.
///
/// SwiftPM gives a test no reliable working directory, and the conformance
/// vectors and the reference server both live in the repository rather than in
/// the package.
enum Repo {
    static let root: URL = {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // strips TestSupport.swift
            .deletingLastPathComponent()  // AuruPMTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // swift
            .deletingLastPathComponent()  // sdk
    }()

    static func read(_ relativePath: String) throws -> Data {
        try Data(contentsOf: root.appendingPathComponent(relativePath))
    }

    static func readText(_ relativePath: String) throws -> String {
        String(decoding: try read(relativePath), as: UTF8.self)
    }
}
