package studio.auru.pm;

import java.util.List;

/**
 * Destructive history policy.
 *
 * <p>There is deliberately no "keep everything" rule: keeping everything means not calling the
 * endpoint. Once a provider has removed a version, changing a preference later cannot bring it
 * back.
 */
public final class Retention {

    private Retention() {}

    /** Which versions survive. */
    public sealed interface Rule {
        /**
         * Keep the newest {@code count} versions.
         *
         * <p>Providers always keep HEAD, including when a malformed client sends zero.
         */
        record Latest(int count) implements Rule {}

        /** Keep HEAD and the history prefix through the oldest commit at or after this time. */
        record Since(long unixSeconds) implements Rule {}
    }

    /**
     * A retention request.
     *
     * <p>{@code protectedCommits} and {@code protectedBlobs} carry in-flight client work — a queued
     * mirror push, a pre-merge stash — that a provider must preserve even when it falls outside the
     * new boundary.
     */
    public record Request(
            Rule rule, List<ContentHash> protectedCommits, List<ContentHash> protectedBlobs) {

        public Request {
            protectedCommits = List.copyOf(protectedCommits);
            protectedBlobs = List.copyOf(protectedBlobs);
        }

        public Request(Rule rule) {
            this(rule, List.of(), List.of());
        }

        Json toJson() {
            Json.ObjectBuilder ruleJson = Json.object();
            if (rule instanceof Rule.Latest latest) {
                ruleJson.put("policy", "latest").put("count", latest.count());
            } else if (rule instanceof Rule.Since since) {
                ruleJson.put("policy", "since").put("timestamp", since.unixSeconds());
            } else {
                throw new IllegalStateException("unreachable: " + rule.getClass());
            }

            Json.ObjectBuilder builder = Json.object().put("rule", ruleJson.build());
            if (!protectedCommits.isEmpty()) {
                builder.put(
                        "protected_commits",
                        Json.array(protectedCommits.stream().map(h -> Json.of(h.toString())).toList()));
            }
            if (!protectedBlobs.isEmpty()) {
                builder.put(
                        "protected_blobs",
                        Json.array(protectedBlobs.stream().map(h -> Json.of(h.toString())).toList()));
            }
            return builder.build();
        }
    }

    /**
     * What one retention pass reclaimed.
     *
     * @param objectsRemoved may be zero even when versions were removed: a provider is allowed to
     *     keep newly orphaned objects for a grace period
     */
    public record Report(long versionsRemoved, long objectsRemoved, long bytesFreed) {
        static Report fromJson(Json json) {
            return new Report(
                    json.integer("versions_removed"),
                    json.integer("objects_removed"),
                    json.integer("bytes_freed"));
        }
    }
}
