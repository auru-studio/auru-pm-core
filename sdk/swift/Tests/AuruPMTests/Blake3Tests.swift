import XCTest

@testable import AuruPM

/// BLAKE3 against the published vectors.
///
/// The vector file is read with a regular expression rather than through this
/// package's own JSON parser, so a failure here means the hash is wrong and
/// nothing else. Two components that can only be tested together can only be
/// debugged together.
final class Blake3Tests: XCTestCase {

    private struct Vector {
        let length: Int
        let hash: String
    }

    private func vectors() throws -> [Vector] {
        let text = try Repo.readText("spec/vectors/content-hash.json")
        let pattern = #"\{\s*"hash":\s*"(blake3:[0-9a-f]{64})",\s*"length":\s*(\d+)\s*\}"#
        let regex = try NSRegularExpression(pattern: pattern)
        let range = NSRange(text.startIndex..., in: text)

        var found: [Vector] = []
        for match in regex.matches(in: text, range: range) {
            guard let hashRange = Range(match.range(at: 1), in: text),
                let lengthRange = Range(match.range(at: 2), in: text),
                let length = Int(text[lengthRange])
            else { continue }
            found.append(Vector(length: length, hash: String(text[hashRange])))
        }
        XCTAssertGreaterThanOrEqual(found.count, 30, "expected the published vectors")
        return found
    }

    /// Byte `i` is `i % 251`, the pattern BLAKE3's own test suite uses.
    private func input(_ length: Int) -> [UInt8] {
        (0..<length).map { UInt8($0 % 251) }
    }

    func testReproducesEveryPublishedVector() throws {
        for vector in try vectors() {
            XCTAssertEqual(
                ContentHash.of(input(vector.length)).description,
                vector.hash,
                "input of \(vector.length) bytes")
        }
    }

    func testCoversTheChunkAndTreeBoundaries() throws {
        // A port that only handles one chunk would pass a careless vector list.
        let lengths = Set(try vectors().map(\.length))
        for boundary in [64, 1024, 1025, 2048, 4096] {
            XCTAssertTrue(lengths.contains(boundary), "missing the \(boundary)-byte boundary")
        }
        XCTAssertTrue(lengths.contains { $0 > 65536 })
    }

    func testHashesTheEmptyInputToTheKnownValue() {
        XCTAssertEqual(
            ContentHash.of("").description,
            "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")
    }

    func testParsesAndRendersTheCanonicalForm() throws {
        let text = "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        let parsed = try ContentHash(parsing: text)
        XCTAssertEqual(parsed.description, text)
        XCTAssertEqual(parsed, ContentHash.of(""))
    }

    func testRejectsAnythingThatIsNotTheCanonicalForm() {
        XCTAssertThrowsError(try ContentHash(parsing: "deadbeef"))
        XCTAssertThrowsError(
            try ContentHash(
                parsing: "sha256:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"))
        // Uppercase would compare unequal as a string to the same hash in lowercase.
        XCTAssertThrowsError(
            try ContentHash(
                parsing: "blake3:AF1349B9F5F9A1A6A0404DEA36DCC9499BCB25C9ADC112B7CC9A93CAE41F3262"))
    }

    func testVerifiesBytesAgainstTheirOwnName() {
        let audio = Array("kick.wav".utf8)
        let hash = ContentHash.of(audio)
        XCTAssertTrue(hash.matches(audio))
        XCTAssertFalse(hash.matches(Array("snare.wav".utf8)))
    }
}
