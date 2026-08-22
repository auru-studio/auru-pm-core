package studio.auru.pm;

import java.util.Optional;
import java.util.OptionalInt;

/**
 * A compare-and-swap on HEAD that lost.
 *
 * <p>Carries the provider's actual HEAD rather than a message, which is the whole point of the
 * shape: a client that lost the race can rebase immediately instead of spending another round trip
 * asking what it lost to.
 */
public final class HeadConflictException extends AuruException {

    private static final long serialVersionUID = 1L;

    private final transient Optional<ContentHash> current;

    HeadConflictException(Optional<ContentHash> current) {
        super(
                ErrorCode.HEAD_CONFLICT,
                "HEAD moved since it was last read"
                        + current.map(head -> "; the provider is now at " + head).orElse(""),
                OptionalInt.of(409),
                Optional.empty(),
                null);
        this.current = current;
    }

    /** The provider's current HEAD. Empty on a project with no commits yet. */
    public Optional<ContentHash> current() {
        return current;
    }
}
