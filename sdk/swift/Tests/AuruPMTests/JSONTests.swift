import XCTest

@testable import AuruPM

/// The hand-written JSON layer.
///
/// Everything else in this package rests on it, and the interesting cases are
/// the ones a canonical encoder has to get exactly right rather than merely
/// parse: escapes, control characters, astral-plane text, and the boundary
/// between an integer and a number that is not one.
final class JSONTests: XCTestCase {

    func testParsesTheShapesAWireBodyUses() throws {
        let value = try JSON.parse(
            #"{"a":1,"b":"two","c":[1,2,3],"d":{"e":true},"f":null,"g":false}"#)
        XCTAssertEqual(try value.integer("a"), 1)
        XCTAssertEqual(try value.string("b"), "two")
        XCTAssertEqual(value["c"]?.arrayValue.count, 3)
        XCTAssertTrue(value["d"]?.bool("e", default: false) ?? false)
        // JSON null reads as absent: a caller never distinguishes "missing"
        // from "explicitly null" when neither carries information.
        XCTAssertNil(value["f"])
        XCTAssertFalse(value.bool("g", default: true))
    }

    func testIgnoresInsignificantWhitespace() throws {
        XCTAssertEqual(
            try JSON.parse(#"{"a":1}"#).canonicalJSON(),
            try JSON.parse("  {\n  \"a\" :\t1\r\n}  ").canonicalJSON())
    }

    func testRoundTripsEveryEscape() throws {
        let text = "quote \" backslash \\ slash / tab \t newline \n return \r"
        let value = JSON.object([("s", .string(text))])
        XCTAssertEqual(try JSON.parse(try value.canonicalJSON()).string("s"), text)
    }

    func testEscapesOnlyWhatMustBeEscaped() throws {
        // Escaping more would still be valid JSON and still be the wrong bytes.
        XCTAssertEqual(try JSON.object([("s", .string("a/b"))]).canonicalJSON(), #"{"s":"a/b"}"#)
        XCTAssertEqual(try JSON.object([("s", .string("caf\u{e9}"))]).canonicalJSON(),
                       "{\"s\":\"caf\u{e9}\"}")
    }

    func testEscapesControlCharactersAsLowercaseHex() throws {
        let controls = "\u{00}\u{1f}"
        XCTAssertEqual(
            try JSON.object([("s", .string(controls))]).canonicalJSON(),
            "{\"s\":\"\\u0000\\u001f\"}")
    }

    func testRoundTripsAstralPlaneTextThroughSurrogateEscapes() throws {
        let astral = "\u{1D541}\u{1D552}"
        XCTAssertEqual(try JSON.parse(#"{"s":"𝕁𝕒"}"#).string("s"), astral)
        let written = JSON.object([("s", .string(astral))]).jsonText()
        XCTAssertEqual(try JSON.parse(written).string("s"), astral)
    }

    func testSortsObjectMembersByUTF16CodeUnit() throws {
        let value = JSON.object([("b", .int(1)), ("a", .int(2)), ("C", .int(3))])
        // Uppercase sorts before lowercase, which is what comparing code units
        // gives and what RFC 8785 requires.
        XCTAssertEqual(try value.canonicalJSON(), #"{"C":3,"a":2,"b":1}"#)
    }

    func testSortsAnAstralKeyTheWayUTF16Does() throws {
        // U+FFFD sorts after U+10000 by code point, but before it in UTF-16,
        // because an astral character leads with a surrogate in the D800 range.
        // Swift's own `<` on String is neither ordering, which is exactly why
        // the canonicalizer compares code units explicitly.
        XCTAssertTrue(JSON.utf16Less("\u{10000}", "\u{FFFD}"))
        XCTAssertFalse("\u{10000}" < "\u{FFFD}")

        let value = JSON.object([("\u{10000}", .int(1)), ("\u{FFFD}", .int(2))])
        let canonical = try value.canonicalJSON()
        XCTAssertLessThan(
            canonical.range(of: "\u{10000}")!.lowerBound,
            canonical.range(of: "\u{FFFD}")!.lowerBound)
    }

    func testKeepsInsertionOrderForOrdinaryJSONText() {
        XCTAssertEqual(JSON.object([("b", .int(1)), ("a", .int(2))]).jsonText(), #"{"b":1,"a":2}"#)
    }

    func testDistinguishesAnIntegerFromANumberThatIsNot() throws {
        XCTAssertEqual(try JSON.parse("1"), .int(1))
        XCTAssertEqual(try JSON.parse("-1"), .int(-1))
        XCTAssertEqual(try JSON.parse("1.0"), .double(1.0))
        XCTAssertEqual(try JSON.parse("1e3"), .double(1000))
        // The distinction is the whole reason canonical encoding can refuse the
        // second kind instead of approximating it.
        XCTAssertThrowsError(try JSON.parse("1.5").canonicalJSON())
    }

    func testRejectsMalformedInput() {
        for malformed in ["", "{", "[1,", #"{"a"}"#, #"{"a":}"#, "tru", "\"unterminated", "{} extra"] {
            XCTAssertThrowsError(try JSON.parse(malformed), "accepted \(malformed)")
        }
    }

    func testRejectsAnUnescapedControlCharacterInAString() {
        XCTAssertThrowsError(try JSON.parse("{\"s\":\"a\nb\"}"))
    }

    func testReportsWhereItGaveUp() {
        XCTAssertThrowsError(try JSON.parse(#"{"a":1,}"#)) { error in
            XCTAssertTrue("\(error)".contains("offset"))
        }
    }

    func testNamesAMissingRequiredMember() {
        XCTAssertThrowsError(try JSON.emptyObject.string("message")) { error in
            XCTAssertTrue("\(error)".contains("message"))
        }
    }

    func testOmitsAbsentOptionalMembersRatherThanWritingNull() throws {
        // A profile with no genre must not carry `"genre":null`; the wire shape
        // distinguishes omitted from present-and-empty.
        XCTAssertEqual(try JSON.object([("a", .int(1)), ("b", nil)]).canonicalJSON(), #"{"a":1}"#)
    }
}
