import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

@testable import AuruPM

/// A transport for the tests, over `URLSession`.
///
/// The live tests use this rather than the shipped `URLSessionTransport` so that
/// the core package's own test target does not depend on the adapter product —
/// which is also how a caller would write their own.
struct SocketTransport: Transport {
    private let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        return URLSession(configuration: configuration)
    }()

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        guard let url = URL(string: request.url) else {
            throw AuruError(code: .badRequest, message: "not a URL: \(request.url)")
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = request.method
        for header in request.headers {
            urlRequest.setValue(header.value, forHTTPHeaderField: header.name)
        }
        urlRequest.httpBody = request.body

        let (data, response) = try await session.data(for: urlRequest)
        guard let http = response as? HTTPURLResponse else {
            throw AuruError(code: .internalError, message: "no HTTP response")
        }
        var headers: [(name: String, value: String)] = []
        for (name, value) in http.allHeaderFields {
            if let name = name as? String, let value = value as? String {
                headers.append((name: name, value: value))
            }
        }
        return HTTPResponse(status: http.statusCode, headers: headers, body: data)
    }
}

/// A transport that answers whatever a test tells it to.
///
/// Providers are written by third parties, so the interesting cases are the ones
/// outside the table — an invented code, or a proxy answering with an HTML error
/// page. Those are ordinary, not hypothetical, and only a stub can produce them
/// on demand.
final class StubTransport: Transport, @unchecked Sendable {

    private let handler: @Sendable (HTTPRequest) -> HTTPResponse
    private let lock = NSLock()
    private var _requests: [HTTPRequest] = []

    var requests: [HTTPRequest] {
        lock.withLock { _requests }
    }

    init(handler: @escaping @Sendable (HTTPRequest) -> HTTPResponse) {
        self.handler = handler
    }

    func send(_ request: HTTPRequest) async throws -> HTTPResponse {
        // Scoped locking rather than lock/unlock: the latter is unavailable
        // from an async context, because a suspension between the two would
        // hold the lock across an await.
        lock.withLock { _requests.append(request) }
        return handler(request)
    }

    static func json(_ status: Int, _ body: String, headers: [(name: String, value: String)] = [])
        -> HTTPResponse
    {
        HTTPResponse(
            status: status,
            headers: headers + [(name: "content-type", value: "application/json")],
            body: Data(body.utf8))
    }

    static func raw(_ status: Int, _ body: String) -> HTTPResponse {
        HTTPResponse(status: status, body: Data(body.utf8))
    }
}

let stubHealth = """
    {"protocol":"auru-pm-v1","provider_id":"stub","capabilities":\
    {"project_listing":true,"members":false,"permissions":false,"branches":false,\
    "server_side_merge":false,"history_retention":false,"project_scoped_blobs":true,\
    "auth_methods":["none"]}}
    """
