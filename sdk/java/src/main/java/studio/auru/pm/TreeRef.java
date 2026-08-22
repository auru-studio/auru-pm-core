package studio.auru.pm;

import java.util.Objects;

/**
 * What a commit points at.
 *
 * @param snapshot blob holding the canonical project JSON for this version
 * @param samples blob listing the {@code (path, hash)} pairs the project depends on. Samples are
 *     fetched lazily, so this is the cheap "what do I need before playback" probe.
 */
public record TreeRef(ContentHash snapshot, ContentHash samples) {

    public TreeRef {
        Objects.requireNonNull(snapshot, "snapshot");
        Objects.requireNonNull(samples, "samples");
    }

    static TreeRef fromJson(Json json) {
        return new TreeRef(
                ContentHash.parse(json.string("snapshot")), ContentHash.parse(json.string("samples")));
    }

    Json toJson() {
        return Json.object()
                .put("snapshot", snapshot.toString())
                .put("samples", samples.toString())
                .build();
    }
}
