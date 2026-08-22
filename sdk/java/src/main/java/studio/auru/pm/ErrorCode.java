package studio.auru.pm;

import java.util.Optional;

/**
 * The closed set of error codes a provider may return.
 *
 * <p>Closed on purpose: a caller can switch over it exhaustively, and a provider that invents a
 * code outside the table is not conformant. When one arrives anyway, {@link #fromWire} maps it by
 * HTTP status rather than inventing a constant.
 */
public enum ErrorCode {
    BAD_REQUEST("bad_request"),
    UNAUTHORIZED("unauthorized"),
    FORBIDDEN("forbidden"),
    NOT_FOUND("not_found"),
    HEAD_CONFLICT("head_conflict"),
    UNSUPPORTED("unsupported"),
    RATE_LIMITED("rate_limited"),
    STORAGE_ERROR("storage_error"),
    INTERNAL("internal"),
    /**
     * The identity provider was unreachable, so the token was never judged.
     *
     * <p>Distinct from {@link #UNAUTHORIZED} because the right response is to retry, not to prompt
     * someone to sign in again.
     */
    AUTHENTICATION_UNAVAILABLE("authentication_unavailable");

    private final String wire;

    ErrorCode(String wire) {
        this.wire = wire;
    }

    /** The value as it appears in a provider's JSON body. */
    public String wireValue() {
        return wire;
    }

    /** Whether retrying the identical request could succeed. */
    public boolean retryable() {
        return this == RATE_LIMITED
                || this == STORAGE_ERROR
                || this == INTERNAL
                || this == AUTHENTICATION_UNAVAILABLE;
    }

    static Optional<ErrorCode> fromWire(String value) {
        for (ErrorCode code : values()) {
            if (code.wire.equals(value)) {
                return Optional.of(code);
            }
        }
        return Optional.empty();
    }

    /** The fallback when a provider sends no usable code — a proxy's HTML 502, say. */
    static ErrorCode forStatus(int status) {
        switch (status) {
            case 400:
                return BAD_REQUEST;
            case 401:
                return UNAUTHORIZED;
            case 403:
                return FORBIDDEN;
            case 404:
                return NOT_FOUND;
            case 409:
                return HEAD_CONFLICT;
            case 422:
                return UNSUPPORTED;
            case 429:
                return RATE_LIMITED;
            case 503:
                return AUTHENTICATION_UNAVAILABLE;
            default:
                return status >= 500 ? INTERNAL : BAD_REQUEST;
        }
    }
}
