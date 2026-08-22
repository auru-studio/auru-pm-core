package studio.auru.pm;

import java.time.Duration;
import java.util.Optional;
import java.util.OptionalInt;

/**
 * Something a provider refused, or that this client refused before sending.
 *
 * <p>Unchecked so that ordinary call sites read normally; the codes worth branching on are worth
 * branching on deliberately, via {@link #code()}, rather than because the compiler insisted.
 */
public class AuruException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    private final ErrorCode code;
    private final transient OptionalInt status;
    private final transient Optional<Duration> retryAfter;

    AuruException(ErrorCode code, String message) {
        this(code, message, OptionalInt.empty(), Optional.empty(), null);
    }

    AuruException(ErrorCode code, String message, Throwable cause) {
        this(code, message, OptionalInt.empty(), Optional.empty(), cause);
    }

    AuruException(
            ErrorCode code,
            String message,
            OptionalInt status,
            Optional<Duration> retryAfter,
            Throwable cause) {
        super(message, cause);
        this.code = code;
        this.status = status;
        this.retryAfter = retryAfter;
    }

    public ErrorCode code() {
        return code;
    }

    /** The HTTP status, absent when this client refused the call before sending it. */
    public OptionalInt status() {
        return status;
    }

    /** How long to wait, from {@code Retry-After}. Only ever present on a rate limit. */
    public Optional<Duration> retryAfter() {
        return retryAfter;
    }

    /** Whether retrying the identical request could succeed. */
    public boolean retryable() {
        return code.retryable();
    }
}
