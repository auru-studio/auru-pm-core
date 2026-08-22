import AuruPM
import Foundation
import XCTest

@testable import AuruPMKernel

/// The native kernel, exercised through the C ABI.
///
/// Two things are being checked. That the binding is correct — the vectors
/// again, because a marshalling bug produces plausible-looking wrong bytes. And
/// that it delivers what the pure client cannot: snapshot normalization, diff,
/// and merge on device.
final class KernelTests: XCTestCase {

    private var repoRoot: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // strips KernelTests.swift
            .deletingLastPathComponent()  // AuruPMKernelTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // swift-kernel
            .deletingLastPathComponent()  // sdk
    }

    private func fixture() throws -> Data {
        try Data(
            contentsOf: repoRoot.appendingPathComponent(
                "crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject"))
    }

    private func vectors() throws -> [JSON] {
        let text = try String(
            contentsOf: repoRoot.appendingPathComponent("spec/vectors/commit-encoding.json"),
            encoding: .utf8)
        return try JSON.parse(text)["cases"]?.arrayValue ?? []
    }

    // MARK: - The binding is correct

    func testAgreesWithTheClientOnTheProtocolVersion() {
        // A binary artifact and a source package can drift out of step in a way
        // neither notices otherwise.
        XCTAssertEqual(Kernel.protocolVersion, auruProtocolVersion)
        XCTAssertTrue(Kernel.matchesClientProtocol)
    }

    func testReproducesEveryPublishedCommitVector() throws {
        let cases = try vectors()
        XCTAssertGreaterThanOrEqual(cases.count, 10)

        for testCase in cases {
            let name = try testCase.string("name")
            let commitJSON = try XCTUnwrap(testCase["commit"]).jsonText()

            let id = try Kernel.commitID(fromJSON: commitJSON)
            XCTAssertEqual(id.description, try testCase.string("id"), name)

            let canonical = try Kernel.canonicalEncoding(fromJSON: commitJSON)
            XCTAssertEqual(
                String(decoding: canonical, as: UTF8.self), try testCase.string("canonical"), name)
        }
    }

    func testTheKernelAndThePureClientDeriveTheSameID() throws {
        // Two implementations of one specification. If these ever disagree, a
        // phone with the kernel and a phone without would publish commits the
        // other could not verify.
        for testCase in try vectors() {
            let json = try XCTUnwrap(testCase["commit"])
            let viaClient = try Commit(json: json)
            let viaKernel = try Kernel.commitID(fromJSON: json.jsonText())
            let name = try testCase.string("name")
            XCTAssertEqual(viaKernel, viaClient.id, name)
        }
    }

    func testHashesAgreeWithThePureClient() throws {
        for length in [0, 1, 63, 64, 65, 1024, 1025, 4096] {
            let bytes = Data((0..<length).map { UInt8($0 % 251) })
            XCTAssertEqual(try Kernel.contentHash(of: bytes), ContentHash.of(bytes), "\(length)")
        }
    }

    func testSurfacesAnErrorRatherThanReturningNonsense() {
        XCTAssertThrowsError(try Kernel.commitID(fromJSON: "not json")) { error in
            XCTAssertTrue("\(error)".contains("parse commit"), "\(error)")
        }
    }

    func testVerifiesBlobsAndRejectsAMalformedHash() throws {
        let bytes = Data("kick.wav".utf8)
        XCTAssertTrue(try Kernel.verifyBlob(bytes, matches: ContentHash.of(bytes)))
        XCTAssertFalse(try Kernel.verifyBlob(Data("snare.wav".utf8), matches: ContentHash.of(bytes)))
    }

    // MARK: - What the pure client cannot do

    func testNormalizesARealProjectFile() throws {
        let source = try fixture()
        let format = try Kernel.detectFormat(fileName: "song.dawproject", source: source)
        XCTAssertEqual(format, "dawproject")

        let snapshot = try Kernel.snapshot(fromSource: source, format: format)
        XCTAssertGreaterThan(snapshot.count, 0)

        // Round-tripping must not change identity.
        let restored = try Kernel.restore(fromSnapshot: snapshot)
        let again = try Kernel.snapshot(fromSource: restored, format: format)
        XCTAssertEqual(try Kernel.contentHash(of: again), try Kernel.contentHash(of: snapshot))
    }

    func testSummarizesAProjectFarMoreCheaplyThanTheSnapshot() throws {
        let source = try fixture()
        let snapshot = try Kernel.snapshot(fromSource: source, format: "dawproject")
        let info = try XCTUnwrap(try Kernel.projectInfo(fromSnapshot: snapshot))

        XCTAssertEqual(info.format, "dawproject")
        // The reason a commit stores this separately: a project list renders
        // without ever fetching a snapshot.
        XCTAssertLessThan(info.detail.jsonText().count, snapshot.count)
    }

    func testReportsNoStructuralChangeBetweenIdenticalSnapshots() throws {
        let snapshot = try Kernel.snapshot(fromSource: try fixture(), format: "dawproject")
        XCTAssertEqual(try Kernel.summarize(before: snapshot, after: snapshot), ["No structural changes"])
    }

    func testReturnsAStructuredDiffARendererCanWalk() throws {
        let snapshot = try Kernel.snapshot(fromSource: try fixture(), format: "dawproject")
        let diff = try Kernel.diff(before: snapshot, after: snapshot)
        XCTAssertNotNil(diff["project_changes"])
        XCTAssertNotNil(diff["channels"])
        XCTAssertEqual(diff["time_sig"]?.arrayValue.count, 2)
    }

    func testMergesDisjointEditsCleanly() throws {
        let outcome = try Kernel.merge(
            ancestor: Data(#"{"version":8,"tempo":120,"key":"C"}"#.utf8),
            local: Data(#"{"version":8,"tempo":128,"key":"C"}"#.utf8),
            remote: Data(#"{"version":8,"tempo":120,"key":"G"}"#.utf8))

        XCTAssertEqual(outcome["outcome"]?.stringValue, "clean")
        XCTAssertEqual(outcome["merged"]?["tempo"]?.intValue, 128)
        XCTAssertEqual(outcome["merged"]?["key"]?.stringValue, "G")
    }

    func testNamesTheFieldWhenBothSidesChangedItDifferently() throws {
        let outcome = try Kernel.merge(
            ancestor: Data(#"{"version":8,"tempo":120}"#.utf8),
            local: Data(#"{"version":8,"tempo":128}"#.utf8),
            remote: Data(#"{"version":8,"tempo":140}"#.utf8))

        XCTAssertEqual(outcome["outcome"]?.stringValue, "conflict")
        XCTAssertEqual(outcome["conflicts"]?.arrayValue.first?["path"]?.stringValue, "tempo")
    }

    func testRefusesAFormatItDoesNotKnowByName() {
        XCTAssertThrowsError(
            try Kernel.snapshot(fromSource: Data("{}".utf8), format: "logic-pro")
        ) { error in
            XCTAssertTrue("\(error)".contains("unknown project format"), "\(error)")
        }
    }

    // MARK: - Memory

    func testRepeatedCallsDoNotLeakOrCorrupt() throws {
        // Every returned buffer is freed on both the success and failure paths.
        // A double free or a missed free shows up here rather than in an app.
        let snapshot = try Kernel.snapshot(fromSource: try fixture(), format: "dawproject")
        for _ in 0..<500 {
            _ = try Kernel.projectInfo(fromSnapshot: snapshot)
            _ = try? Kernel.commitID(fromJSON: "not json")
            _ = try Kernel.contentHash(of: snapshot)
        }
        XCTAssertEqual(
            try Kernel.contentHash(of: snapshot), ContentHash.of(snapshot),
            "the kernel drifted after repeated calls")
    }
}
