import Foundation
import XCTest

@testable import AuruPM

/// End-to-end against a real `auru-pm-server`.
final class TransportTests: XCTestCase {

    private static var provider: TestProvider?

    private func connect() async throws -> AuruClient {
        if TransportTests.provider == nil {
            TransportTests.provider = try await TestProvider()
        }
        return try await AuruClient.connect(
            endpoint: TransportTests.provider!.endpoint, transport: SocketTransport())
    }

    private func fixture() throws -> Data {
        try Repo.read("crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject")
    }

    /// Create a handle. A provider only knows about one once its profile exists.
    private func createProject(_ client: AuruClient, _ handle: String, _ name: String) async throws
        -> ProjectClient
    {
        let project = try await client.project(handle)
        try await project.putProfile(ProjectProfile(displayName: name, format: "dawproject"))
        return project
    }

    /// Build, upload and record a version the way a real client would.
    @discardableResult
    private func publish(
        _ client: AuruClient, _ project: ProjectClient, _ message: String,
        parents: [ContentHash] = []
    ) async throws -> Commit {
        let identity = try await client.me()
        let snapshot = try await project.putBlob(try fixture())
        let samples = try await project.putBlob(Data("{\"entries\":[]}".utf8))

        let commit = try Commit(
            parents: parents,
            tree: TreeRef(snapshot: snapshot, samples: samples),
            author: identity.asAuthor,
            timestamp: 1_700_000_000,
            message: message,
            auruVersion: "0.1.0",
            formatVersion: 1)

        let stored = try await project.putCommit(commit)
        XCTAssertEqual(stored, commit.id)
        return commit
    }

    func testReportsTheProvidersProtocolAndCapabilities() async throws {
        let client = try await connect()
        let health = await client.health
        let capabilities = await client.capabilities
        XCTAssertEqual(health.protocolVersion, "auru-pm-v1")
        XCTAssertEqual(health.providerID, "swift-sdk-test")
        XCTAssertTrue(capabilities.projectListing)
    }

    func testAcceptsAnEndpointWithATrailingSlash() async throws {
        _ = try await connect()
        let client = try await AuruClient.connect(
            endpoint: TransportTests.provider!.endpoint + "/", transport: SocketTransport())
        let endpoint = await client.endpoint
        XCTAssertEqual(endpoint, TransportTests.provider!.endpoint)
    }

    func testRefusesAnEndpointThatIsNotHTTP() async throws {
        do {
            _ = try await AuruClient.connect(
                endpoint: "ftp://pm.example.com", transport: SocketTransport())
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .badRequest)
        }
    }

    func testPublishesAVersionAndReadsItBack() async throws {
        let client = try await connect()
        let project = try await createProject(client, "life-cycle", "Life Cycle")

        let empty = try await project.head()
        XCTAssertNil(empty)

        let commit = try await publish(client, project, "first take")
        try await project.advanceHead(from: nil, to: commit.id)
        let head = try await project.head()
        XCTAssertEqual(head, commit.id)

        let fetched = try await project.getCommit(commit.id)
        XCTAssertEqual(fetched.message, "first take")
        // Round-tripping through the provider must not disturb identity.
        XCTAssertTrue(fetched.verifyID())

        let history = try await project.history()
        XCTAssertEqual(history.map(\.message), ["first take"])
    }

    func testStoresAProfileAndListsTheProject() async throws {
        let client = try await connect()
        let project = try await createProject(client, "catalogued", "placeholder")
        let commit = try await publish(client, project, "catalogued")
        try await project.advanceHead(from: nil, to: commit.id)

        try await project.putProfile(
            ProjectProfile(
                displayName: "Night Drive", format: "dawproject",
                genre: "Drum & Bass, Jungle", tags: ["wip"]))

        let listed = try await client.listProjects().first { $0.handle == "catalogued" }
        XCTAssertEqual(listed?.profile?.displayName, "Night Drive")
        XCTAssertEqual(listed?.profile?.tags, ["wip"])
    }

    func testPagesHistoryNewestFirst() async throws {
        let client = try await connect()
        let project = try await createProject(client, "paged", "Paged")

        var parent: ContentHash?
        var newest: ContentHash?
        for message in ["one", "two", "three"] {
            let commit = try await publish(
                client, project, message, parents: parent.map { [$0] } ?? [])
            try await project.advanceHead(from: parent, to: commit.id)
            parent = commit.id
            newest = commit.id
        }

        let all = try await project.history()
        XCTAssertEqual(all.map(\.message), ["three", "two", "one"])
        let limited = try await project.history(limit: 2)
        XCTAssertEqual(limited.count, 2)
        let older = try await project.history(before: newest)
        XCTAssertEqual(older.map(\.message), ["two", "one"])
    }

    func testReportsTheActualHeadWhenTheCallersIsStale() async throws {
        let client = try await connect()
        let project = try await createProject(client, "racing", "Racing")

        let first = try await publish(client, project, "first")
        try await project.advanceHead(from: nil, to: first.id)
        let second = try await publish(client, project, "second", parents: [first.id])
        try await project.advanceHead(from: first.id, to: second.id)

        // A client that still believes HEAD is `first` loses the race, and has
        // to learn what it lost to without another round trip.
        let stale = try await publish(client, project, "stale", parents: [first.id])
        do {
            try await project.advanceHead(from: first.id, to: stale.id)
            XCTFail("expected a conflict")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .headConflict)
            XCTAssertEqual(error.currentHead, second.id)
            XCTAssertFalse(error.isRetryable)
        }
    }

    func testVerifiesADownloadAgainstItsOwnName() async throws {
        let client = try await connect()
        let project = try await createProject(client, "blobs", "Blobs")
        let audio = Data("kick.wav pretending to be audio".utf8)
        let hash = ContentHash.of(audio)

        let absent = try await project.hasBlobs([hash])
        XCTAssertEqual(absent, [false])
        let stored = try await project.putBlob(audio)
        XCTAssertEqual(stored, hash)
        let present = try await project.hasBlobs([hash])
        XCTAssertEqual(present, [true])
        let fetched = try await project.getBlob(hash)
        XCTAssertEqual(fetched, audio)

        // Re-uploading is not an error.
        _ = try await project.putBlob(audio)
    }

    func testReadsASummaryWithoutFetchingTheSnapshot() async throws {
        let client = try await connect()
        let project = try await createProject(client, "summarized", "Summarized")

        let summary = Data(
            "{\"schema\":1,\"format\":\"dawproject\",\"dawproject\":{\"title\":\"Night Drive\"}}"
                .utf8)
        let summaryHash = try await project.putBlob(summary)
        let snapshot = try await project.putBlob(try fixture())
        let samples = try await project.putBlob(Data("{\"entries\":[]}".utf8))

        let commit = try Commit(
            tree: TreeRef(snapshot: snapshot, samples: samples),
            author: try await client.me().asAuthor,
            timestamp: 1_700_000_000,
            message: "with a summary",
            auruVersion: "0.1.0",
            formatVersion: 1,
            metadata: summaryHash)
        try await project.putCommit(commit)

        let info = try await project.projectInfo(for: commit)
        XCTAssertEqual(info?.format, "dawproject")
        XCTAssertEqual(info?.text("title"), "Night Drive")
        let snapshotSize = try fixture().count
        XCTAssertLessThan(summary.count, snapshotSize)
    }

    func testRefusesACommitWhoseIDDoesNotMatchItsContent() async throws {
        // No initializer can produce one, so this goes through the wire type —
        // which is what a broken or hostile client would do.
        let client = try await connect()
        let project = try await createProject(client, "tampered", "Tampered")
        let snapshot = try await project.putBlob(try fixture())
        let samples = try await project.putBlob(Data("{\"entries\":[]}".utf8))
        let author = try await client.me().asAuthor

        let forged = JSON.object([
            ("id", .string("blake3:" + String(repeating: "a", count: 64))),
            ("parents", .array([])),
            ("tree", TreeRef(snapshot: snapshot, samples: samples).json),
            ("author", author.json),
            ("timestamp", .int(1_700_000_000)),
            ("message", .string("an id I did not compute")),
            ("description", .string("")),
            ("auru_version", .string("0.1.0")),
            ("format_version", .int(1)),
        ])

        let commit = try Commit(json: forged)
        XCTAssertFalse(commit.verifyID())

        do {
            try await project.putCommit(commit)
            XCTFail("expected the provider to reject it")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .badRequest)
        }
    }

    func testReportsAMissingCommitAsNotFound() async throws {
        let client = try await connect()
        let project = try await client.project("life-cycle")
        do {
            _ = try await project.getCommit(
                try ContentHash(parsing: "blake3:" + String(repeating: "b", count: 64)))
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .notFound)
        }
    }

    func testRejectsAnEmptyProjectHandle() async throws {
        let client = try await connect()
        do {
            _ = try await client.project("")
            XCTFail("expected a failure")
        } catch let error as AuruError {
            XCTAssertEqual(error.code, .badRequest)
        }
    }

    func testPrunesHistoryWhenTheProviderSupportsIt() async throws {
        let client = try await connect()
        let retention = await client.capabilities.historyRetention
        XCTAssertTrue(retention)
        let project = try await createProject(client, "pruned", "Pruned")

        var parent: ContentHash?
        for message in ["one", "two", "three", "four"] {
            let commit = try await publish(
                client, project, message, parents: parent.map { [$0] } ?? [])
            try await project.advanceHead(from: parent, to: commit.id)
            parent = commit.id
        }
        let before = try await project.history()
        XCTAssertEqual(before.count, 4)

        let report = try await project.pruneHistory(RetentionRequest(rule: .latest(count: 2)))
        XCTAssertEqual(report.versionsRemoved, 2)
        let after = try await project.history()
        XCTAssertEqual(after.count, 2)
    }
}
