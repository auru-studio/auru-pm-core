package studio.auru.pm;

import java.util.ArrayList;
import java.util.List;
import java.util.Optional;

/**
 * A provider's public OAuth configuration.
 *
 * <p>Endpoint URLs are deliberately absent — a client discovers them from the exact issuer via
 * OpenID Connect discovery or RFC 8414 metadata. A provider that could name its own token endpoint
 * could name someone else's.
 */
public record OAuthConfiguration(
        String issuer, String audience, String requiredScope, List<OAuthClient> clients) {

    /** One registered public client. */
    public record OAuthClient(Kind kind, String clientId, String redirectUri, List<String> flows) {
        public OAuthClient {
            flows = List.copyOf(flows);
        }
    }

    /**
     * Which kind of client a registration is for.
     *
     * <p>Not cosmetic: each kind uses different redirect rules and they must not share a client id,
     * because an identity provider's redirect allow-list is per client.
     */
    public enum Kind {
        /** Desktop or CLI, redirecting to an exact loopback URI. */
        NATIVE,
        /** Single-page app, redirecting to an https URL it serves itself. */
        BROWSER,
        /** Phone app, redirecting to a custom scheme or an https universal link it owns. */
        MOBILE,
        /**
         * A kind this build does not know. Kept rather than mislabelled, so a registration for a
         * client newer than this SDK is never mistaken for one of the kinds above.
         */
        UNKNOWN
    }

    public OAuthConfiguration {
        clients = List.copyOf(clients);
    }

    /** The registration of a given kind, if the provider published one. */
    public Optional<OAuthClient> client(Kind kind) {
        return clients.stream().filter(client -> client.kind() == kind).findFirst();
    }

    static OAuthConfiguration fromJson(Json json) {
        List<OAuthClient> clients = new ArrayList<>();
        for (Json client : json.get("clients").orElse(Json.NULL).elements()) {
            clients.add(
                    new OAuthClient(
                            switch (client.string("kind")) {
                                case "native" -> Kind.NATIVE;
                                case "browser" -> Kind.BROWSER;
                                case "mobile" -> Kind.MOBILE;
                                default -> Kind.UNKNOWN;
                            },
                            client.string("client_id"),
                            client.string("redirect_uri"),
                            client.require("flows").elements().stream()
                                    .map(value -> ((Json.Str) value).value())
                                    .toList()));
        }

        // The singular fields predate `clients` and describe the native client.
        // Reading them keeps a provider written before the list existed usable.
        if (clients.stream().noneMatch(client -> client.kind() == Kind.NATIVE)) {
            Optional<String> clientId = json.optString("client_id");
            Optional<String> redirectUri = json.optString("redirect_uri");
            if (clientId.isPresent() && redirectUri.isPresent()) {
                clients.add(
                        0,
                        new OAuthClient(
                                Kind.NATIVE,
                                clientId.get(),
                                redirectUri.get(),
                                json.get("flows").orElse(Json.NULL).elements().stream()
                                        .map(value -> ((Json.Str) value).value())
                                        .toList()));
            }
        }

        return new OAuthConfiguration(
                json.string("issuer"),
                json.string("audience"),
                json.optString("required_scope").orElse("openid"),
                clients);
    }
}
