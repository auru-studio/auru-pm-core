import Foundation

/// The closed set of error codes a provider may return.
///
/// Closed on purpose: a caller can `switch` over it exhaustively and the
/// compiler will say when a case is missing. A provider that invents a code
/// outside the table is not conformant; when one arrives anyway it is mapped by
/// HTTP status instead.
public enum ErrorCode: String, Sendable, CaseIterable {
    case badRequest = "bad_request"
    case unauthorized
    case forbidden
    case notFound = "not_found"
    case headConflict = "head_conflict"
    case unsupported
    case rateLimited = "rate_limited"
    case storageError = "storage_error"
    case internalError = "internal"
    /// The identity provider was unreachable, so the token was never judged.
    ///
    /// Distinct from `unauthorized` because the right response is to retry
    /// rather than to prompt someone to sign in again.
    case authenticationUnavailable = "authentication_unavailable"

    /// Whether retrying the identical request could succeed.
    public var isRetryable: Bool {
        switch self {
        case .rateLimited, .storageError, .internalError, .authenticationUnavailable:
            return true
        case .badRequest, .unauthorized, .forbidden, .notFound, .headConflict, .unsupported:
            return false
        }
    }

    /// The fallback when a provider sends no usable code — a proxy's HTML 502.
    static func forStatus(_ status: Int) -> ErrorCode {
        switch status {
        case 400: return .badRequest
        case 401: return .unauthorized
        case 403: return .forbidden
        case 404: return .notFound
        case 409: return .headConflict
        case 422: return .unsupported
        case 429: return .rateLimited
        case 503: return .authenticationUnavailable
        default: return status >= 500 ? .internalError : .badRequest
        }
    }
}

/// Something a provider refused, or that this client refused before sending.
public struct AuruError: Error, Sendable, CustomStringConvertible {

    public let code: ErrorCode
    public let message: String
    /// The HTTP status, absent when the client refused the call before sending.
    public let status: Int?
    /// How long to wait, from `Retry-After`. Only ever set on a rate limit.
    public let retryAfter: TimeInterval?
    /// The provider's actual HEAD, set only on `headConflict`.
    ///
    /// Carried on the error rather than fetched afterwards so a client that lost
    /// a compare-and-swap can rebase without another round trip.
    public let currentHead: ContentHash?

    public init(
        code: ErrorCode,
        message: String,
        status: Int? = nil,
        retryAfter: TimeInterval? = nil,
        currentHead: ContentHash? = nil
    ) {
        self.code = code
        self.message = message
        self.status = status
        self.retryAfter = retryAfter
        self.currentHead = currentHead
    }

    /// Whether retrying the identical request could succeed.
    public var isRetryable: Bool { code.isRetryable }

    public var description: String {
        var text = "\(code.rawValue): \(message)"
        if let status { text += " (HTTP \(status))" }
        return text
    }
}
