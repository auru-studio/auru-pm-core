package studio.auru.pm;

import java.util.Optional;

/**
 * Who a provider says you are, derived from the presented token.
 *
 * <p>The identity key is {@code (issuer, subject)}, never email. A commit's author must match this,
 * so read it once and reuse it rather than composing an author from local settings.
 */
public record AuthenticatedIdentity(
        String providerId, String userId, String displayName, Optional<String> email) {

    static AuthenticatedIdentity fromJson(Json json) {
        return new AuthenticatedIdentity(
                json.string("provider_id"),
                json.string("user_id"),
                json.string("display_name"),
                json.optString("email"));
    }

    /** An {@link AuthorIdentity} for commits written by this identity. */
    public AuthorIdentity asAuthor() {
        return new AuthorIdentity(displayName, userId, providerId, email);
    }
}
