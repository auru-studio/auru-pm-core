package studio.auru.pm;

import java.nio.charset.StandardCharsets;
import java.util.Optional;

/**
 * The few-kilobyte summary of what a version is.
 *
 * <p>A real Live Set snapshot is around 7 MB and roughly a hundred thousand elements. Every commit
 * also stores this — tempo, key, tracks, plugins — and points at it with {@link Commit#metadata()},
 * so a project list can render without ever fetching a snapshot.
 *
 * <p>The per-DAW body is exposed as raw {@link Json} rather than mapped to a record. Each adapter
 * grows fields as it learns to read more of a format, and a Java type would have to be republished
 * before a dashboard could display one.
 */
public record ProjectInfo(int schema, String format, Json detail) {

    /** Parse a stored ProjectInfo blob. */
    public static ProjectInfo parse(byte[] blob) {
        return fromJson(Json.parse(new String(blob, StandardCharsets.UTF_8)));
    }

    static ProjectInfo fromJson(Json json) {
        String format = json.string("format");
        Json detail =
                json.get("ableton")
                        .or(() -> json.get("flstudio"))
                        .or(() -> json.get("dawproject"))
                        .orElse(Json.NULL);
        return new ProjectInfo((int) json.integer("schema"), format, detail);
    }

    /** A field of the per-DAW body, e.g. {@code "tempo"} or {@code "title"}. */
    public Optional<Json> field(String name) {
        return detail.get(name);
    }

    /** A text field of the per-DAW body, empty when absent or not a string. */
    public Optional<String> text(String name) {
        return detail.get(name).flatMap(Json::asText);
    }

    /** A numeric field of the per-DAW body, e.g. {@code "tempo"}. */
    public Optional<Double> number(String name) {
        return detail.get(name).flatMap(Json::asNumber);
    }
}
