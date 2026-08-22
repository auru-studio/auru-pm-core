import Foundation
import XCTest

@testable import AuruPM

/// How this client behaves when a provider misbehaves.
///
/// Providers are written by third parties, so the interesting cases are the ones
/// outside the table — an invented code, or a proxy answering with an HTML error
/// page. Those are ordinary, not hypothetical.
final class ErrorMappingTests: XCTestCase {

    private func stub(_ reply: @escaping @Sendable () -> HTTPResponse) async throws
        -> (AuruClient, StubTransport)
    {
        let transport = StubTransport { request in
            request.url.hasSuffix("/v1/health") ? StubTransport.json(200, stubHealth) : reply()
        }
        let client = try await AuruClient.connect(
            endpoint: "http://pm.example.com", transport: transport)
        return (client, transport)
    }

    func testKeepsTheCodeAndMessageAProviderSent() async throws {
        let (client, _) = try await stub {
            StubTransport.json(403, #"{"code":"forbidden","message":"not your project"}"#)
        }
        do {
            _ = try await client.me()
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .forbidden)
            XCTAssertEqual(error.message, "not your project")
            XCTAssertEqual(error.status, 403)
            XCTAssertFalse(error.isRetryable)
        }
    }

    func testSurfacesHeadConflictWithTheCurrentHead() async throws {
        let current = "blake3:" + String(repeating: "c", count: 64)
        let (client, _) = try await stub {
            StubTransport.json(409, "{\"code\":\"head_conflict\",\"current\":\"\(current)\"}")
        }
        let project = try await client.project("p")
        do {
            try await project.advanceHead(
                from: nil,
                to: try ContentHash(parsing: "blake3:" + String(repeating: "d", count: 64)))
            XCTFail("expected a conflict")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .headConflict)
            XCTAssertEqual(error.currentHead?.description, current)
        }
    }

    func testReadsANullCurrentHeadOnAnEmptyProject() async throws {
        let (client, _) = try await stub {
            StubTransport.json(409, #"{"code":"head_conflict","current":null}"#)
        }
        let project = try await client.project("p")
        do {
            try await project.advanceHead(
                from: nil,
                to: try ContentHash(parsing: "blake3:" + String(repeating: "f", count: 64)))
            XCTFail("expected a conflict")
        } catch let error as AuruError {
            XCTAssertNil(error.currentHead)
        }
    }

    func testCarriesRetryAfterOnARateLimit() async throws {
        let (client, _) = try await stub {
            StubTransport.json(
                429, #"{"code":"rate_limited","message":"slow down"}"#,
                headers: [(name: "Retry-After", value: "60")])
        }
        do {
            _ = try await client.me()
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .rateLimited)
            XCTAssertEqual(error.retryAfter, 60)
            XCTAssertTrue(error.isRetryable)
        }
    }

    func testTreatsAnUnreachableIdentityProviderAsRetryable() async throws {
        // Prompting someone to sign in again here would be wrong: the token was
        // never judged.
        let (client, _) = try await stub {
            StubTransport.json(503, #"{"code":"authentication_unavailable","message":"idp down"}"#)
        }
        do {
            _ = try await client.me()
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .authenticationUnavailable)
            XCTAssertTrue(error.isRetryable)
        }
    }

    func testFallsBackToTheStatusWhenAProviderInventsACode() async throws {
        let (client, _) = try await stub {
            StubTransport.json(404, #"{"code":"teapot","message":"unusual"}"#)
        }
        do {
            _ = try await client.me()
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .notFound)
            XCTAssertEqual(error.message, "unusual")
        }
    }

    func testSurvivesAProxyAnsweringWithHTML() async throws {
        let (client, _) = try await stub { StubTransport.raw(502, "<html>502 Bad Gateway</html>") }
        do {
            _ = try await client.me()
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .internalError)
            XCTAssertTrue(error.isRetryable)
            XCTAssertTrue(error.message.contains("502"))
        }
    }

    func testRefusesAProviderSpeakingADifferentWireVersion() async throws {
        let transport = StubTransport { _ in
            StubTransport.json(
                200, stubHealth.replacingOccurrences(of: "auru-pm-v1", with: "auru-pm-v2"))
        }
        do {
            _ = try await AuruClient.connect(endpoint: "http://pm.example.com", transport: transport)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unsupported)
            XCTAssertTrue(error.message.contains("auru-pm-v2"))
        }
    }

    func testRefusesAnUnadvertisedCapabilityBeforeARoundTrip() async throws {
        let (client, transport) = try await stub { StubTransport.json(200, "{}") }
        let retention = await client.capabilities.historyRetention
        XCTAssertFalse(retention)

        let project = try await client.project("p")
        do {
            _ = try await project.pruneHistory(RetentionRequest(rule: .latest(count: 10)))
            XCTFail("expected a refusal")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .unsupported)
        }
        // Nothing was sent: only the health request ever reached the stub.
        XCTAssertEqual(transport.requests.count, 1)
    }

    func testSendsTheBearerTokenAndNeverExposesIt() async throws {
        let transport = StubTransport { request in
            request.url.hasSuffix("/v1/health")
                ? StubTransport.json(200, stubHealth)
                : StubTransport.json(200, #"{"commit_id":null}"#)
        }
        let client = try await AuruClient.connect(
            endpoint: "http://pm.example.com", transport: transport, accessToken: "secret-token")
        let authenticated = await client.isAuthenticated
        XCTAssertTrue(authenticated)

        let project = try await client.project("p")
        _ = try await project.head()
        XCTAssertTrue(
            transport.requests.last!.headers.contains {
                $0.name == "authorization" && $0.value == "Bearer secret-token"
            })

        await client.setAccessToken(nil)
        _ = try await project.head()
        XCTAssertFalse(transport.requests.last!.headers.contains { $0.name == "authorization" })
    }

    func testVerifiesADownloadedBlobAndRejectsTamperedBytes() async throws {
        let (client, _) = try await stub { StubTransport.raw(200, "not the bytes you asked for") }
        let expected = ContentHash.of("the real bytes")
        let project = try await client.project("p")
        do {
            _ = try await project.getBlob(expected)
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertTrue(error.message.contains("does not hash to its own name"))
        }
    }

    func testAskingAboutNoBlobsIsNotARequest() async throws {
        let (client, transport) = try await stub { StubTransport.json(500, #"{"code":"internal"}"#) }
        let project = try await client.project("p")
        let none = try await project.hasBlobs([])
        XCTAssertTrue(none.isEmpty)
        XCTAssertEqual(transport.requests.count, 1)
    }
}
