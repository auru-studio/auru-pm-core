package studio.auru.pm;

import java.util.Objects;
import java.util.Optional;

/**
 * Who made a version, captured at commit time.
 *
 * <p>Recorded inline rather than looked up so history renders without a live provider connection,
 * and so attribution survives someone leaving a workspace.
 *
 * <p>A provider checks these against the identity it derived from the bearer token and rejects a
 * mismatch, so build one from {@link AuruClient#me()} rather than from local settings.
 */
public record AuthorIdentity(
        String displayName, String providerUserId, String providerId, Optional<String> email) {

    public AuthorIdentity {
        Objects.requireNonNull(displayName, "displayName");
        Objects.requireNonNull(providerUserId, "providerUserId");
        Objects.requireNonNull(providerId, "providerId");
        Objects.requireNonNull(email, "email");
    }

    public AuthorIdentity(String displayName, String providerUserId, String providerId) {
        this(displayName, providerUserId, providerId, Optional.empty());
    }

    static AuthorIdentity fromJson(Json json) {
        return new AuthorIdentity(
                json.string("display_name"),
                json.string("provider_user_id"),
                json.string("provider_id"),
                json.optString("email"));
    }

    Json toJson() {
        return Json.object()
                .put("display_name", displayName)
                .put("provider_user_id", providerUserId)
                .put("provider_id", providerId)
                .putIfPresent("email", email.map(Json::of).orElse(null))
                .build();
    }
}
