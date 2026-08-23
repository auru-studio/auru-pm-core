import Foundation
import XCTest

@testable import AuruPM

/// Reading the published provider list.
///
/// The rule this suite is really about is what happens when the list is *not*
/// there. A picker that invents a plausible provider to fill the gap offers the
/// user something they cannot connect to, and they find that out several taps
/// later. Failing is the useful answer, because the screen above it has a
/// perfectly good address field to fall back to.
final class ProviderRegistryTests: XCTestCase {

    /// What `https://pm.auru.studio/providers.json` actually served, byte for
    /// byte, on 2026-08-23. Pinned as a fixture so a change in the published
    /// document shows up here as a failing test rather than as a first-run
    /// screen that quietly renders nothing.
    private let published = """
        {"providers":[{"id":"auru-cloud","name":"Auru Cloud",\
        "endpoint":"https://pm.auru.studio",\
        "detail":"Hosted · au-melb · encrypted at rest",\
        "description":"Hosted backup with encrypted storage",\
        "auth_methods":["oauth_device_code"],"recommended":true}]}
        """

    func testReadsThePublishedRegistry() throws {
        let registry = try ProviderRegistry.parse(Data(published.utf8))

        XCTAssertEqual(registry.providers.count, 1)
        let entry = try XCTUnwrap(registry.providers.first)
        XCTAssertEqual(entry.id, "auru-cloud")
        XCTAssertEqual(entry.name, "Auru Cloud")
        XCTAssertEqual(entry.endpoint, "https://pm.auru.studio")
        XCTAssertEqual(entry.detail, "Hosted · au-melb · encrypted at rest")
        XCTAssertEqual(entry.description, "Hosted backup with encrypted storage")
        XCTAssertEqual(entry.authMethods, ["oauth_device_code"])
        XCTAssertTrue(entry.recommended)
        XCTAssertNil(entry.iconURL)
    }

    func testTreatsEverythingButIdentityAndAddressAsOptional() throws {
        // A registry is hand-maintained. An entry that describes itself sparsely
        // is still an entry somebody can connect to.
        let sparse = """
            {"providers":[{"id":"minimal","name":"Minimal",\
            "endpoint":"https://minimal.example.com"}]}
            """
        let entry = try XCTUnwrap(try ProviderRegistry.parse(Data(sparse.utf8)).providers.first)

        XCTAssertEqual(entry.description, "")
        XCTAssertEqual(entry.detail, "")
        XCTAssertEqual(entry.authMethods, [])
        XCTAssertFalse(entry.recommended)
    }

    func testAnEmptyRegistryIsARegistry() throws {
        // Different from a registry that could not be fetched, and the first-run
        // screen says something different for each.
        let registry = try ProviderRegistry.parse(Data(#"{"providers":[]}"#.utf8))

        XCTAssertEqual(registry.providers, [])
    }

    func testRefusesAnEntryWithNowhereToConnectTo() throws {
        let broken = #"{"providers":[{"id":"broken","name":"Broken"}]}"#

        XCTAssertThrowsError(try ProviderRegistry.parse(Data(broken.utf8))) { error in
            XCTAssertTrue("\(error)".contains("endpoint"), "\(error)")
        }
    }

    func testRefusesADocumentThatIsNotARegistryAtAll() throws {
        // What a captive portal or a misconfigured CDN actually returns.
        XCTAssertThrowsError(try ProviderRegistry.parse(Data("<html>Not found</html>".utf8))) {
            error in
            XCTAssertTrue("\(error)".contains("not JSON"), "\(error)")
        }
    }

    func testFetchesOverTheTransport() async throws {
        let body = published
        let transport = StubTransport { _ in StubTransport.json(200, body) }

        let registry = try await ProviderRegistry.fetch(
            from: ProviderRegistry.auruRegistryURL, transport: transport)

        XCTAssertEqual(registry.providers.first?.id, "auru-cloud")
    }

    func testReportsARegistryThatAnswersWithAnError() async throws {
        let transport = StubTransport { _ in StubTransport.raw(503, "") }

        do {
            _ = try await ProviderRegistry.fetch(
                from: ProviderRegistry.auruRegistryURL, transport: transport)
            XCTFail("expected the error status to be reported")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("503"), error.message)
        }
    }
}
