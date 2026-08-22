package studio.auru.pm;

import java.util.List;

/**
 * What a provider implements.
 *
 * <p>Every flag defaults to false, which is what makes capabilities safe to add: a provider written
 * before one existed omits it and a client reads it as absent rather than guessing.
 */
public record Capabilities(
        boolean projectListing,
        boolean members,
        boolean permissions,
        boolean branches,
        boolean serverSideMerge,
        boolean compressedUploads,
        boolean historyRetention,
        boolean projectScopedBlobs,
        List<String> authMethods) {

    public Capabilities {
        authMethods = List.copyOf(authMethods);
    }

    static Capabilities fromJson(Json json) {
        return new Capabilities(
                json.bool("project_listing", false),
                json.bool("members", false),
                json.bool("permissions", false),
                json.bool("branches", false),
                json.bool("server_side_merge", false),
                json.bool("compressed_uploads", false),
                json.bool("history_retention", false),
                json.bool("project_scoped_blobs", false),
                json.get("auth_methods").orElse(Json.NULL).elements().stream()
                        .map(value -> ((Json.Str) value).value())
                        .toList());
    }
}
