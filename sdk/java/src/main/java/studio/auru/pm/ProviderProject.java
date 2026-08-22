package studio.auru.pm;

import java.util.Optional;

/**
 * One project visible to the authenticated account.
 *
 * @param profile absent for a project written before catalogues existed; a client then reads the
 *     HEAD snapshot for its format and falls back to the handle as a display name
 * @param updatedAt Unix epoch seconds of the HEAD commit
 */
public record ProviderProject(
        String handle, ContentHash head, Optional<ProjectProfile> profile, long updatedAt) {

    static ProviderProject fromJson(Json json) {
        return new ProviderProject(
                json.string("handle"),
                ContentHash.parse(json.string("head")),
                json.get("profile").map(ProjectProfile::fromJson),
                json.integer("updated_at"));
    }
}
