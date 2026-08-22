package studio.auru.pm;

/** Constants for {@code auru-pm-v1}. */
public final class Protocol {

    /**
     * The wire protocol version this client speaks.
     *
     * <p>A client refuses to talk to a provider whose {@code /v1/health} reports anything else: a
     * mismatch means the shapes it serves are not the shapes below, and finding that out at the
     * first commit rather than at connect time would be worse.
     */
    public static final String VERSION = "auru-pm-v1";

    private Protocol() {}
}
