package studio.auru.pm;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Objects;
import java.util.Optional;

/**
 * One version in a project's history.
 *
 * <p>{@code parents().size()} is the shape: 0 root, 1 normal, 2 merge.
 *
 * <p>The id is the BLAKE3 of the RFC 8785 canonicalization of every other field. Providers
 * recompute it and treat a mismatch as auth-equivalent — a client writing what it did not compute —
 * so there is no constructor that takes an id. {@link Builder#build()} derives it, and
 * {@link #fromJson} keeps the one a provider sent while {@link #verifyId()} checks it.
 */
public final class Commit {

    private final ContentHash id;
    private final List<ContentHash> parents;
    private final TreeRef tree;
    private final AuthorIdentity author;
    private final long timestamp;
    private final String message;
    private final String description;
    private final String auruVersion;
    private final long formatVersion;
    private final Optional<ContentHash> metadata;

    private Commit(
            ContentHash id,
            List<ContentHash> parents,
            TreeRef tree,
            AuthorIdentity author,
            long timestamp,
            String message,
            String description,
            String auruVersion,
            long formatVersion,
            Optional<ContentHash> metadata) {
        this.id = id;
        this.parents = List.copyOf(parents);
        this.tree = tree;
        this.author = author;
        this.timestamp = timestamp;
        this.message = message;
        this.description = description;
        this.auruVersion = auruVersion;
        this.formatVersion = formatVersion;
        this.metadata = metadata;
    }

    public ContentHash id() {
        return id;
    }

    public List<ContentHash> parents() {
        return parents;
    }

    public TreeRef tree() {
        return tree;
    }

    public AuthorIdentity author() {
        return author;
    }

    /** Unix epoch seconds. */
    public long timestamp() {
        return timestamp;
    }

    public String message() {
        return message;
    }

    public String description() {
        return description;
    }

    public String auruVersion() {
        return auruVersion;
    }

    /** Snapshot schema version at commit time, so a reader knows whether it needs migration. */
    public long formatVersion() {
        return formatVersion;
    }

    /**
     * Blob holding this commit's {@link ProjectInfo} — a few kilobytes of tempo, key, tracks and
     * plugins.
     *
     * <p>This is what lets a project list render without fetching the snapshot, which for a real
     * Live Set is around 7 MB. Absent on older commits and on formats the writer could not
     * summarize; a reader that finds it missing falls back to the snapshot.
     */
    public Optional<ContentHash> metadata() {
        return metadata;
    }

    public static Builder builder() {
        return new Builder();
    }

    /**
     * The exact bytes this commit's id is the BLAKE3 of.
     *
     * <p>Worth having when a provider rejects a commit: comparing these against
     * {@code spec/vectors/commit-encoding.json} says immediately which side is wrong.
     */
    public byte[] canonicalEncoding() {
        return canonicalEncoding(toJsonWithoutId());
    }

    private static byte[] canonicalEncoding(Json withoutId) {
        return withoutId.toCanonicalJson().getBytes(StandardCharsets.UTF_8);
    }

    /** Whether {@link #id()} matches this commit's content. */
    public boolean verifyId() {
        return ContentHash.of(canonicalEncoding()).equals(id);
    }

    private Json toJsonWithoutId() {
        List<Json> parentJson = new ArrayList<>(parents.size());
        for (ContentHash parent : parents) {
            parentJson.add(Json.of(parent.toString()));
        }
        return Json.object()
                .put("parents", Json.array(parentJson))
                .put("tree", tree.toJson())
                .put("author", author.toJson())
                .put("timestamp", timestamp)
                .put("message", message)
                .put("description", description)
                .put("auru_version", auruVersion)
                .put("format_version", formatVersion)
                .putIfPresent("metadata", metadata.map(hash -> Json.of(hash.toString())).orElse(null))
                .build();
    }

    Json toJson() {
        Json.ObjectBuilder builder = Json.object().put("id", id.toString());
        toJsonWithoutId().members().forEach(builder::put);
        return builder.build();
    }

    /** Read a commit as a provider sent it, keeping its id. */
    public static Commit fromJson(Json json) {
        List<ContentHash> parents = new ArrayList<>();
        for (Json parent : json.require("parents").elements()) {
            parents.add(ContentHash.parse(((Json.Str) parent).value()));
        }
        return new Commit(
                ContentHash.parse(json.string("id")),
                parents,
                TreeRef.fromJson(json.require("tree")),
                AuthorIdentity.fromJson(json.require("author")),
                json.integer("timestamp"),
                json.string("message"),
                json.optString("description").orElse(""),
                json.string("auru_version"),
                json.integer("format_version"),
                json.optString("metadata").map(ContentHash::parse));
    }

    @Override
    public String toString() {
        return "Commit[" + id + " \"" + message + "\"]";
    }

    @Override
    public boolean equals(Object other) {
        return other instanceof Commit commit && id.equals(commit.id);
    }

    @Override
    public int hashCode() {
        return id.hashCode();
    }

    /** Assembles a commit and derives its id. */
    public static final class Builder {
        private List<ContentHash> parents = List.of();
        private TreeRef tree;
        private AuthorIdentity author;
        private Long timestamp;
        private String message;
        private String description = "";
        private String auruVersion;
        private Long formatVersion;
        private Optional<ContentHash> metadata = Optional.empty();

        private Builder() {}

        /** Empty for a root version, one for a normal one, two for a merge. */
        public Builder parents(List<ContentHash> parents) {
            this.parents = List.copyOf(parents);
            return this;
        }

        public Builder parent(ContentHash parent) {
            return parents(List.of(parent));
        }

        public Builder tree(TreeRef tree) {
            this.tree = tree;
            return this;
        }

        public Builder author(AuthorIdentity author) {
            this.author = author;
            return this;
        }

        /** Unix epoch seconds. */
        public Builder timestamp(long timestamp) {
            this.timestamp = timestamp;
            return this;
        }

        public Builder message(String message) {
            this.message = message;
            return this;
        }

        public Builder description(String description) {
            this.description = Objects.requireNonNull(description, "description");
            return this;
        }

        public Builder auruVersion(String auruVersion) {
            this.auruVersion = auruVersion;
            return this;
        }

        public Builder formatVersion(long formatVersion) {
            this.formatVersion = formatVersion;
            return this;
        }

        public Builder metadata(ContentHash metadata) {
            this.metadata = Optional.ofNullable(metadata);
            return this;
        }

        /**
         * Derive the id and produce the commit.
         *
         * @throws IllegalStateException if a required field is missing, if there are more than two
         *     parents, or if a number falls outside what RFC 8785 can represent.
         */
        public Commit build() {
            require(tree, "tree");
            require(author, "author");
            require(timestamp, "timestamp");
            require(message, "message");
            require(auruVersion, "auruVersion");
            require(formatVersion, "formatVersion");
            if (parents.size() > 2) {
                throw new IllegalStateException(
                        "a commit has at most two parents (0 root, 1 normal, 2 merge), got "
                                + parents.size());
            }

            Commit unidentified =
                    new Commit(
                            ContentHash.of(new byte[0]),
                            parents,
                            tree,
                            author,
                            timestamp,
                            message,
                            description,
                            auruVersion,
                            formatVersion,
                            metadata);
            ContentHash id = ContentHash.of(canonicalEncoding(unidentified.toJsonWithoutId()));
            return new Commit(
                    id,
                    parents,
                    tree,
                    author,
                    timestamp,
                    message,
                    description,
                    auruVersion,
                    formatVersion,
                    metadata);
        }

        private static void require(Object value, String name) {
            if (value == null) {
                throw new IllegalStateException("a commit needs " + name);
            }
        }
    }
}
