package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Optional;
import org.junit.jupiter.api.Test;

/**
 * Reading the published provider list.
 *
 * <p>The rule this suite is really about is what happens when the list is <em>not</em> there. A
 * picker that invents a plausible provider to fill the gap offers the user something they cannot
 * connect to, and they find that out several taps later. Failing is the useful answer, because the
 * screen above it has a perfectly good address field to fall back to.
 */
class ProviderRegistryTest {

    /** Answers one canned response, or refuses to connect at all. */
    private record Stub(int status, String body, IOException failure) implements Transport {

        static Stub answering(int status, String body) {
            return new Stub(status, body, null);
        }

        static Stub unreachable() {
            return new Stub(0, "", new IOException("no route to host"));
        }

        @Override
        public Transport.Response send(Transport.Request request) throws IOException {
            if (failure != null) {
                throw failure;
            }
            return new Transport.Response(status, List.of(), body.getBytes(StandardCharsets.UTF_8));
        }
    }

    /**
     * What {@code https://pm.auru.studio/providers.json} actually served, byte for byte, on
     * 2026-08-23. Pinned as a fixture so a change in the published document shows up here as a
     * failing test rather than as a first-run screen that quietly renders nothing.
     */
    private static final String PUBLISHED =
            "{\"providers\":[{\"id\":\"auru-cloud\",\"name\":\"Auru Cloud\","
                    + "\"endpoint\":\"https://pm.auru.studio\","
                    + "\"detail\":\"Hosted · au-melb · encrypted at rest\","
                    + "\"description\":\"Hosted backup with encrypted storage\","
                    + "\"auth_methods\":[\"oauth_device_code\"],\"recommended\":true}]}";

    @Test
    void readsThePublishedRegistry() {
        ProviderRegistry registry = ProviderRegistry.parse(PUBLISHED);

        assertEquals(1, registry.providers().size());
        ProviderRegistry.Entry entry = registry.providers().get(0);
        assertEquals("auru-cloud", entry.id());
        assertEquals("Auru Cloud", entry.name());
        assertEquals("https://pm.auru.studio", entry.endpoint());
        assertEquals("Hosted · au-melb · encrypted at rest", entry.detail());
        assertEquals("Hosted backup with encrypted storage", entry.description());
        assertEquals(List.of("oauth_device_code"), entry.authMethods());
        assertTrue(entry.recommended());
        assertEquals(Optional.empty(), entry.iconUrl());
    }

    @Test
    void treatsEverythingButIdentityAndAddressAsOptional() {
        // A registry is hand-maintained. An entry that describes itself sparsely
        // is still an entry somebody can connect to.
        ProviderRegistry registry =
                ProviderRegistry.parse(
                        "{\"providers\":[{\"id\":\"minimal\",\"name\":\"Minimal\","
                                + "\"endpoint\":\"https://minimal.example.com\"}]}");

        ProviderRegistry.Entry entry = registry.providers().get(0);
        assertEquals("", entry.description());
        assertEquals("", entry.detail());
        assertEquals(List.of(), entry.authMethods());
        assertTrue(!entry.recommended());
    }

    @Test
    void anEmptyRegistryIsARegistry() {
        // Different from a registry that could not be fetched, and the first-run
        // screen says something different for each.
        assertEquals(List.of(), ProviderRegistry.parse("{\"providers\":[]}").providers());
    }

    @Test
    void refusesAnEntryWithNowhereToConnectTo() {
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                ProviderRegistry.parse(
                                        "{\"providers\":[{\"id\":\"broken\",\"name\":\"Broken\"}]}"));

        assertTrue(error.getMessage().contains("endpoint"), error.getMessage());
    }

    @Test
    void refusesADocumentThatIsNotARegistryAtAll() {
        // What a captive portal or a misconfigured CDN actually returns.
        AuruException error =
                assertThrows(
                        AuruException.class, () -> ProviderRegistry.parse("<html>Not found</html>"));

        assertTrue(error.getMessage().contains("not JSON"), error.getMessage());
    }

    @Test
    void fetchesOverTheTransport() {
        ProviderRegistry registry =
                ProviderRegistry.fetch(
                        ProviderRegistry.AURU_REGISTRY_URL, Stub.answering(200, PUBLISHED));

        assertEquals("auru-cloud", registry.providers().get(0).id());
    }

    @Test
    void reportsAnUnreachableRegistryRatherThanInventingOne() {
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                ProviderRegistry.fetch(
                                        ProviderRegistry.AURU_REGISTRY_URL, Stub.unreachable()));

        assertTrue(error.getMessage().contains("cannot reach"), error.getMessage());
    }

    @Test
    void reportsARegistryThatAnswersWithAnError() {
        AuruException error =
                assertThrows(
                        AuruException.class,
                        () ->
                                ProviderRegistry.fetch(
                                        ProviderRegistry.AURU_REGISTRY_URL, Stub.answering(503, "")));

        assertTrue(error.getMessage().contains("503"), error.getMessage());
    }
}
