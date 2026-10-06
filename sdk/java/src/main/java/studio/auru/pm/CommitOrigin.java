package studio.auru.pm;

/**
 * Why a commit was made, when it was not a person pressing Save Version.
 *
 * <p>A commit without an origin is a version a person saved. The member is left out of the wire
 * form entirely when absent, so every explicit save keeps the id it was created with.
 */
public enum CommitOrigin {
    /** Written by the app on a timer or at session end, not asked for. */
    AUTOSAVE("autosave");

    private final String wireName;

    CommitOrigin(String wireName) {
        this.wireName = wireName;
    }

    /** The value as it appears on a commit. */
    public String wireName() {
        return wireName;
    }

    /**
     * The origin a wire value names.
     *
     * @throws IllegalArgumentException for a value this library does not know
     */
    public static CommitOrigin fromWireName(String wireName) {
        for (CommitOrigin origin : values()) {
            if (origin.wireName.equals(wireName)) {
                return origin;
            }
        }
        throw new IllegalArgumentException("unknown commit origin \"" + wireName + "\"");
    }
}
