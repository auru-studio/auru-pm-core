package studio.auru.pm;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;

/**
 * A published list of providers, for the "which backup do you use?" step of first run.
 *
 * <p>A registry is a convenience, never an authority. It says a provider exists and where to find
 * it; everything that matters — the protocol version, the capabilities, which authentication
 * methods actually work — comes from that provider's own {@code GET /v1/health} once connected. An
 * entry here is a suggestion the user is free to ignore, and any endpoint they type by hand is
 * equally valid.
 *
 * <p><strong>A registry that cannot be fetched produces an error, never a list.</strong> {@link
 * #fetch} throws rather than returning something partial or invented. Filling the gap with a
 * plausible default would put entries in front of someone that they cannot connect to, which is
 * strictly worse than showing nothing and offering them the address field.
 */
public record ProviderRegistry(List<Entry> providers) {

    /** The Auru-curated registry. */
    public static final String AURU_REGISTRY_URL = "https://pm.auru.studio/providers.json";

    public ProviderRegistry {
        providers = List.copyOf(providers);
    }

    /**
     * One provider, as the registry describes it.
     *
     * @param authMethods what signing in will involve — worth showing before the user commits,
     *     because "you will be sent to your browser" and "go and create a token somewhere" are very
     *     different amounts of work. Empty when the entry claims nothing.
     */
    public record Entry(
            String id,
            String name,
            String endpoint,
            String description,
            String detail,
            List<String> authMethods,
            boolean recommended,
            Optional<String> iconUrl) {

        public Entry {
            authMethods = List.copyOf(authMethods);
        }
    }

    /**
     * Fetch and parse a registry.
     *
     * @throws AuruException if it cannot be reached, does not answer 200, or is not a registry
     */
    public static ProviderRegistry fetch(String url, Transport transport) {
        Transport.Response response;
        try {
            response =
                    transport.send(
                            new Transport.Request(
                                    "GET", url, List.of(Map.entry("accept", "application/json")), null));
        } catch (IOException cause) {
            throw new AuruException(ErrorCode.INTERNAL, "cannot reach " + url + ": " + cause, cause);
        }

        if (response.status() != 200) {
            throw new AuruException(
                    ErrorCode.INTERNAL, url + " answered HTTP " + response.status());
        }
        return parse(new String(response.body(), StandardCharsets.UTF_8));
    }

    /**
     * Parse a registry document.
     *
     * <p>A malformed entry fails the whole document rather than being skipped. A registry is
     * hand-maintained, and silently dropping the one provider somebody just added to it is a bug
     * report nobody can reproduce.
     *
     * @throws AuruException if the document is not JSON, or an entry omits what it must have
     */
    public static ProviderRegistry parse(String document) {
        Json json;
        try {
            json = Json.parse(document);
        } catch (RuntimeException notJson) {
            throw new AuruException(
                    ErrorCode.INTERNAL, "the provider registry is not JSON: " + notJson.getMessage());
        }

        List<Entry> entries = new ArrayList<>();
        for (Json entry : json.get("providers").orElse(Json.NULL).elements()) {
            entries.add(
                    new Entry(
                            required(entry, "id"),
                            required(entry, "name"),
                            required(entry, "endpoint"),
                            entry.optString("description").orElse(""),
                            entry.optString("detail").orElse(""),
                            entry.get("auth_methods").orElse(Json.NULL).elements().stream()
                                    .filter(Json.Str.class::isInstance)
                                    .map(value -> ((Json.Str) value).value())
                                    .toList(),
                            entry.get("recommended").orElse(Json.NULL) instanceof Json.Bool flag
                                    && flag.value(),
                            entry.optString("icon_url")));
        }
        return new ProviderRegistry(entries);
    }

    private static String required(Json entry, String field) {
        return entry.optString(field)
                .filter(value -> !value.isEmpty())
                .orElseThrow(
                        () ->
                                new AuruException(
                                        ErrorCode.INTERNAL,
                                        "a provider registry entry has no " + field));
    }
}
