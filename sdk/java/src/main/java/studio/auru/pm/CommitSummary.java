package studio.auru.pm;

import java.util.ArrayList;
import java.util.List;

/**
 * A history row.
 *
 * <p>Omits {@link TreeRef} so listing history does not force a tree fetch per row.
 */
public record CommitSummary(
        ContentHash id,
        List<ContentHash> parents,
        AuthorIdentity author,
        long timestamp,
        String message,
        String description) {

    public CommitSummary {
        parents = List.copyOf(parents);
    }

    static CommitSummary fromJson(Json json) {
        List<ContentHash> parents = new ArrayList<>();
        for (Json parent : json.require("parents").elements()) {
            parents.add(ContentHash.parse(((Json.Str) parent).value()));
        }
        return new CommitSummary(
                ContentHash.parse(json.string("id")),
                parents,
                AuthorIdentity.fromJson(json.require("author")),
                json.integer("timestamp"),
                json.string("message"),
                json.optString("description").orElse(""));
    }
}
