package studio.auru.pm;

import java.util.Optional;

/**
 * What a provider says about itself, before any credential is presented.
 *
 * <p>Always public, so a client can learn which auth methods and capabilities a provider supports
 * before prompting anyone for anything.
 */
public record ProviderHealth(
        String protocol,
        Optional<String> providerId,
        Optional<String> name,
        Capabilities capabilities,
        Optional<OAuthConfiguration> authentication) {

    static ProviderHealth fromJson(Json json) {
        return new ProviderHealth(
                json.string("protocol"),
                json.optString("provider_id"),
                json.optString("name"),
                Capabilities.fromJson(json.require("capabilities")),
                json.get("authentication").map(OAuthConfiguration::fromJson));
    }
}
