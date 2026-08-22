import Foundation

@testable import AuruPM

/// A real `auru-pm-server`, for the end-to-end tests.
///
/// Mocked HTTP would only prove this client agrees with the test's idea of the
/// protocol. Running the reference implementation is the only way to find out
/// whether the two interoperate.
final class TestProvider: @unchecked Sendable {

    let endpoint: String
    private let process: Process
    private let directory: URL

    init() async throws {
        let build = Process()
        build.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        build.arguments = [
            "cargo", "build", "-p", "auru-pm-server", "--locked",
            "--manifest-path", Repo.root.appendingPathComponent("Cargo.toml").path,
        ]
        build.standardOutput = FileHandle.nullDevice
        build.standardError = FileHandle.nullDevice
        try build.run()
        build.waitUntilExit()

        directory = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("auru-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let port = Int.random(in: 4900...5200)
        endpoint = "http://127.0.0.1:\(port)"

        let config = directory.appendingPathComponent("server.toml")
        try """
            version = 1
            provider_id = "swift-sdk-test"
            listen = "127.0.0.1:\(port)"
            data_dir = "\(directory.appendingPathComponent("data").path)"
            requests_per_minute = 100000

            [authentication]
            mode = "none"
            """.write(to: config, atomically: true, encoding: .utf8)

        process = Process()
        process.executableURL = Repo.root.appendingPathComponent("target/debug/auru-pm-server")
        process.arguments = ["--config", config.path]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()

        let transport = SocketTransport()
        for _ in 0..<200 {
            if let response = try? await transport.send(
                HTTPRequest(method: "GET", url: endpoint + "/v1/health")),
                response.status == 200
            {
                return
            }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        throw AuruError(code: .internalError, message: "the provider did not start on \(endpoint)")
    }

    deinit {
        process.terminate()
        try? FileManager.default.removeItem(at: directory)
    }
}
