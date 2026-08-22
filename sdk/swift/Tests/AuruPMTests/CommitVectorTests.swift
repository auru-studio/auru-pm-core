import XCTest

@testable import AuruPM

/// The conformance gate.
///
/// A commit whose id this package cannot reproduce is rejected by every
/// provider, so these are not stylistic assertions — they are the contract. The
/// cases come from `spec/vectors/commit-encoding.json`, generated from the Rust
/// implementation, and the TypeScript, Java and C++ SDKs are checked against the
/// same file.
final class CommitVectorTests: XCTestCase {

    private func vectors() throws -> JSON {
        try JSON.parse(try Repo.readText("spec/vectors/commit-encoding.json"))
    }

    private func cases() throws -> [JSON] {
        let all = try vectors()["cases"]?.arrayValue ?? []
        XCTAssertGreaterThanOrEqual(all.count, 10, "expected the published vectors")
        return all
    }

    func testPublishesTheRuleItIsCheckedAgainst() throws {
        XCTAssertTrue(try vectors().string("rule").contains("RFC 8785"))
    }

    func testReproducesTheCanonicalBytes() throws {
        for testCase in try cases() {
            let name = try testCase.string("name")
            let commit = try Commit(json: XCTUnwrap(testCase["commit"]))
            XCTAssertEqual(
                try commit.canonicalEncoding(), try testCase.string("canonical"),
                "canonical bytes differ for \(name)")
        }
    }

    func testDerivesTheRecordedID() throws {
        for testCase in try cases() {
            let name = try testCase.string("name")
            let commit = try Commit(json: XCTUnwrap(testCase["commit"]))
            XCTAssertEqual(commit.id.description, try testCase.string("id"), name)
            XCTAssertTrue(commit.verifyID(), "id does not verify for \(name)")
        }
    }

    func testIgnoresTheIDAlreadyOnACommit() throws {
        // Identity is a function of content, not of itself.
        let first = try XCTUnwrap(try cases().first)
        let original = try Commit(json: XCTUnwrap(first["commit"]))

        let rebuilt = try Commit(
            parents: original.parents, tree: original.tree, author: original.author,
            timestamp: original.timestamp, message: original.message,
            description: original.description, auruVersion: original.auruVersion,
            formatVersion: original.formatVersion, metadata: original.metadata)
        XCTAssertEqual(rebuilt.id, original.id)
    }

    func testChangesTheIDWhenAnyContentChanges() throws {
        let original = try Commit(json: XCTUnwrap(try cases()[0]["commit"]))
        let edited = try Commit(
            parents: original.parents, tree: original.tree, author: original.author,
            timestamp: original.timestamp, message: "a different message",
            description: original.description, auruVersion: original.auruVersion,
            formatVersion: original.formatVersion)
        XCTAssertNotEqual(edited.id, original.id)
    }

    func testCanonicalEncodingCarriesNoIDMember() throws {
        for testCase in try cases() {
            let commit = try Commit(json: XCTUnwrap(testCase["commit"]))
            XCTAssertFalse(try commit.canonicalEncoding().contains("\"id\""))
        }
    }

    func testJSONRoundTripsThroughInit() throws {
        // `canonicalEncoding` and `json` are different code paths: the first is
        // what a hash is derived from, the second is what goes on the wire. A
        // test that only exercises one leaves the other free to be wrong.
        for testCase in try cases() {
            let original = try Commit(json: XCTUnwrap(testCase["commit"]))
            let reparsed = try Commit(json: JSON.parse(original.json.jsonText()))
            XCTAssertEqual(reparsed.id, original.id)
            XCTAssertTrue(reparsed.verifyID())
            XCTAssertEqual(reparsed.message, original.message)
            XCTAssertEqual(reparsed.parents, original.parents)
            XCTAssertEqual(reparsed.metadata, original.metadata)
        }
    }

    func testCanonicalizingIsIdempotent() throws {
        // Re-canonicalizing already-canonical bytes must change nothing. This
        // separates a parser fault from a writer fault.
        for testCase in try cases() {
            let canonical = try testCase.string("canonical")
            XCTAssertEqual(try JSON.parse(canonical).canonicalJSON(), canonical)
        }
    }

    func testRefusesAnIntegerOutsideWhatRFC8785CanRepresent() {
        // Beyond 2^53 two conformant implementations derive different ids, so
        // producing bytes at all here would be worse than failing.
        let beyond = JSON.object([JSONMember("timestamp", .int(9_007_199_254_740_993))])
        XCTAssertThrowsError(try beyond.canonicalJSON()) { error in
            XCTAssertTrue("\(error)".contains("2^53"))
        }
    }

    func testRefusesToCanonicalizeAFractionalNumber() throws {
        let fractional = try JSON.parse("{\"tempo\":128.5}")
        XCTAssertThrowsError(try fractional.canonicalJSON()) { error in
            XCTAssertTrue("\(error)".contains("non-integral"))
        }
    }

    func testRefusesMoreThanTwoParents() throws {
        let hash = ContentHash.of("x")
        XCTAssertThrowsError(
            try Commit(
                parents: [hash, hash, hash],
                tree: TreeRef(snapshot: hash, samples: hash),
                author: AuthorIdentity(displayName: "T", providerUserID: "u", providerID: "p"),
                timestamp: 0, message: "m", auruVersion: "0.1.0", formatVersion: 1))
    }
}
