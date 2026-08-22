package studio.auru.pm;

import java.util.List;
import java.util.Optional;

/**
 * The human-facing metadata an account project list needs.
 *
 * <p>Registering one is also how a project handle comes into existence: until then every other
 * project-scoped endpoint answers {@code not_found}, blob upload included.
 */
public record ProjectProfile(
        String displayName,
        String format,
        Optional<String> genre,
        List<String> tags,
        Optional<String> relativePath) {

    public ProjectProfile {
        tags = List.copyOf(tags);
    }

    public ProjectProfile(String displayName, String format) {
        this(displayName, format, Optional.empty(), List.of(), Optional.empty());
    }

    static ProjectProfile fromJson(Json json) {
        Json metadata = json.get("metadata").orElse(Json.NULL);
        return new ProjectProfile(
                json.string("display_name"),
                json.string("format"),
                metadata.optString("genre"),
                metadata.get("tags").orElse(Json.NULL).elements().stream()
                        .map(value -> ((Json.Str) value).value())
                        .toList(),
                json.get("location").flatMap(location -> location.optString("relative_path")));
    }

    Json toJson() {
        Json.ObjectBuilder metadata = Json.object();
        genre.ifPresent(value -> metadata.put("genre", value));
        if (!tags.isEmpty()) {
            metadata.put("tags", Json.array(tags.stream().map(Json::of).toList()));
        }
        Json.Obj metadataJson = metadata.build();

        Json.ObjectBuilder builder =
                Json.object().put("display_name", displayName).put("format", format);
        if (!metadataJson.members().isEmpty()) {
            builder.put("metadata", metadataJson);
        }
        relativePath.ifPresent(
                path -> builder.put("location", Json.object().put("relative_path", path).build()));
        return builder.build();
    }
}
