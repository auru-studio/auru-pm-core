import Foundation

/// One HTTP request.
public struct HTTPRequest: Sendable {
    public var method: String
    public var url: String
    public var headers: [(name: String, value: String)]
    public var body: Data?

    public init(
        method: String, url: String, headers: [(name: String, value: String)] = [],
        body: Data? = nil
    ) {
        self.method = method
        self.url = url
        self.headers = headers
        self.body = body
    }
}

/// One HTTP response.
public struct HTTPResponse: Sendable {
    public var status: Int
    public var headers: [(name: String, value: String)]
    public var body: Data

    public init(status: Int, headers: [(name: String, value: String)] = [], body: Data = Data()) {
        self.status = status
        self.headers = headers
        self.body = body
    }

    /// A header by name, matched case-insensitively as HTTP requires.
    public func header(_ name: String) -> String? {
        let wanted = name.lowercased()
        return headers.first { $0.name.lowercased() == wanted }?.value
    }
}

/// How this client reaches a provider.
///
/// A protocol rather than a fixed HTTP stack. `URLSession` is the obvious choice
/// on Apple platforms and is provided as `URLSessionTransport`, but on Linux it
/// lives in a different module, and an application with its own networking layer
/// — its own retry policy, its own certificate pinning, its own instrumentation
/// — should not be made to run a second one.
///
/// Conformances must be safe to use concurrently if the client is shared.
public protocol Transport: Sendable {
    /// Perform one request.
    ///
    /// Throw only when the request could not be completed at all — DNS,
    /// connection, TLS, cancellation. A non-2xx response is an `HTTPResponse`,
    /// not an error: interpreting a provider's status is this library's job, and
    /// a transport that guessed would have to be corrected here anyway.
    func send(_ request: HTTPRequest) async throws -> HTTPResponse
}
