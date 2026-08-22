import AuruPM
import Foundation

#if canImport(FoundationNetworking)
    // On Linux, URLSession lives in a separate module. Importing it only where
    // it exists is what lets the core package stay platform-free.
    import FoundationNetworking
#endif

/// A `Transport` over `URLSession`.
///
/// A separate product so that depending on the core never pulls URLSession in.
public struct URLSessionTransport: Transport {

    private let session: URLSession

    /// - Parameter session: supply a configured session for a proxy, certificate
    ///   pinning, background transfers, or a test.
    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func send(_ request: HTTPRequest) async throws -> HTTPResponse {
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
            throw AuruError(code: .internalError, message: "no HTTP response from \(request.url)")
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
