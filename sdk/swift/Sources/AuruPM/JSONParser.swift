import Foundation

extension JSON {

    /// Parse JSON text.
    ///
    /// - Throws: `AuruError` on anything that is not one well-formed JSON value.
    public static func parse(_ text: String) throws -> JSON {
        var parser = Parser(Array(text.utf8))
        let value = try parser.parseValue()
        parser.skipWhitespace()
        guard parser.atEnd else {
            throw parser.error("trailing content after the JSON value")
        }
        return value
    }

    /// Parse JSON bytes.
    public static func parse(_ data: Data) throws -> JSON {
        try parse(String(decoding: data, as: UTF8.self))
    }

    /// A recursive-descent parser. Small because JSON is small.
    ///
    /// Works over UTF-8 bytes rather than `Character`s: grapheme clusters are the
    /// wrong unit for a format defined in terms of code units, and indexing them
    /// is far slower besides.
    private struct Parser {
        private let bytes: [UInt8]
        private var position = 0

        init(_ bytes: [UInt8]) { self.bytes = bytes }

        var atEnd: Bool { position >= bytes.count }

        func error(_ message: String) -> AuruError {
            AuruError(code: .badRequest, message: "JSON at offset \(position): \(message)")
        }

        mutating func skipWhitespace() {
            while position < bytes.count {
                switch bytes[position] {
                case 0x20, 0x09, 0x0A, 0x0D: position += 1
                default: return
                }
            }
        }

        mutating func parseValue() throws -> JSON {
            skipWhitespace()
            guard position < bytes.count else { throw error("unexpected end of input") }
            switch bytes[position] {
            case UInt8(ascii: "{"): return try parseObject()
            case UInt8(ascii: "["): return try parseArray()
            case UInt8(ascii: "\""): return .string(try parseString())
            case UInt8(ascii: "t"): return try parseLiteral("true", .bool(true))
            case UInt8(ascii: "f"): return try parseLiteral("false", .bool(false))
            case UInt8(ascii: "n"): return try parseLiteral("null", .null)
            default: return try parseNumber()
            }
        }

        private mutating func parseLiteral(_ literal: String, _ value: JSON) throws -> JSON {
            let expected = Array(literal.utf8)
            guard position + expected.count <= bytes.count,
                Array(bytes[position..<(position + expected.count)]) == expected
            else {
                throw error("expected \(literal)")
            }
            position += expected.count
            return value
        }

        private mutating func parseObject() throws -> JSON {
            position += 1  // '{'
            var members: [JSONMember] = []
            skipWhitespace()
            if position < bytes.count, bytes[position] == UInt8(ascii: "}") {
                position += 1
                return .object(members)
            }
            while true {
                skipWhitespace()
                guard position < bytes.count, bytes[position] == UInt8(ascii: "\"") else {
                    throw error("expected a member name")
                }
                let name = try parseString()
                skipWhitespace()
                guard position < bytes.count, bytes[position] == UInt8(ascii: ":") else {
                    throw error("expected ':' after a member name")
                }
                position += 1
                members.append(JSONMember(name, try parseValue()))

                skipWhitespace()
                guard position < bytes.count else { throw error("unterminated object") }
                let next = bytes[position]
                position += 1
                if next == UInt8(ascii: "}") { return .object(members) }
                guard next == UInt8(ascii: ",") else { throw error("expected ',' or '}' in object") }
            }
        }

        private mutating func parseArray() throws -> JSON {
            position += 1  // '['
            var elements: [JSON] = []
            skipWhitespace()
            if position < bytes.count, bytes[position] == UInt8(ascii: "]") {
                position += 1
                return .array(elements)
            }
            while true {
                elements.append(try parseValue())
                skipWhitespace()
                guard position < bytes.count else { throw error("unterminated array") }
                let next = bytes[position]
                position += 1
                if next == UInt8(ascii: "]") { return .array(elements) }
                guard next == UInt8(ascii: ",") else { throw error("expected ',' or ']' in array") }
            }
        }

        private mutating func parseHex4() throws -> UInt32 {
            guard position + 4 <= bytes.count else { throw error("truncated \\u escape") }
            var value: UInt32 = 0
            for _ in 0..<4 {
                let digit = bytes[position]
                position += 1
                switch digit {
                case UInt8(ascii: "0")...UInt8(ascii: "9"):
                    value = value * 16 + UInt32(digit - UInt8(ascii: "0"))
                case UInt8(ascii: "a")...UInt8(ascii: "f"):
                    value = value * 16 + UInt32(digit - UInt8(ascii: "a") + 10)
                case UInt8(ascii: "A")...UInt8(ascii: "F"):
                    value = value * 16 + UInt32(digit - UInt8(ascii: "A") + 10)
                default:
                    throw error("not a hex digit in \\u escape")
                }
            }
            return value
        }

        private mutating func parseString() throws -> String {
            position += 1  // '"'
            var out = [UInt8]()
            while true {
                guard position < bytes.count else { throw error("unterminated string") }
                let byte = bytes[position]
                position += 1

                if byte == UInt8(ascii: "\"") {
                    return String(decoding: out, as: UTF8.self)
                }
                if byte != UInt8(ascii: "\\") {
                    if byte < 0x20 { throw error("unescaped control character in string") }
                    out.append(byte)
                    continue
                }

                guard position < bytes.count else { throw error("truncated escape") }
                let escape = bytes[position]
                position += 1
                switch escape {
                case UInt8(ascii: "\""): out.append(UInt8(ascii: "\""))
                case UInt8(ascii: "\\"): out.append(UInt8(ascii: "\\"))
                case UInt8(ascii: "/"): out.append(UInt8(ascii: "/"))
                case UInt8(ascii: "b"): out.append(0x08)
                case UInt8(ascii: "f"): out.append(0x0C)
                case UInt8(ascii: "n"): out.append(0x0A)
                case UInt8(ascii: "r"): out.append(0x0D)
                case UInt8(ascii: "t"): out.append(0x09)
                case UInt8(ascii: "u"):
                    var scalar = try parseHex4()
                    // A lead surrogate is only half a character; the trail
                    // follows as a second escape.
                    if (0xD800...0xDBFF).contains(scalar), position + 6 <= bytes.count,
                        bytes[position] == UInt8(ascii: "\\"),
                        bytes[position + 1] == UInt8(ascii: "u")
                    {
                        let saved = position
                        position += 2
                        let trail = try parseHex4()
                        if (0xDC00...0xDFFF).contains(trail) {
                            scalar = 0x10000 + ((scalar - 0xD800) << 10) + (trail - 0xDC00)
                        } else {
                            position = saved
                        }
                    }
                    out.append(contentsOf: Array(String(UnicodeScalar(scalar) ?? "\u{FFFD}").utf8))
                default:
                    throw error("unknown escape")
                }
            }
        }

        private mutating func parseNumber() throws -> JSON {
            let start = position
            if position < bytes.count, bytes[position] == UInt8(ascii: "-") { position += 1 }

            var fractional = false
            while position < bytes.count {
                let byte = bytes[position]
                if byte >= UInt8(ascii: "0"), byte <= UInt8(ascii: "9") {
                    position += 1
                } else if byte == UInt8(ascii: ".") || byte == UInt8(ascii: "e")
                    || byte == UInt8(ascii: "E") || byte == UInt8(ascii: "+")
                    || byte == UInt8(ascii: "-")
                {
                    fractional = true
                    position += 1
                } else {
                    break
                }
            }

            let literal = String(decoding: bytes[start..<position], as: UTF8.self)
            guard !literal.isEmpty, literal != "-" else { throw error("expected a value") }

            if !fractional, let value = Int64(literal) {
                return .int(value)
            }
            // A value too large for Int64 falls through to a double, which loses
            // precision — but it is already outside what this protocol permits,
            // and canonicalizing it will say so.
            guard let value = Double(literal) else { throw error("not a number: \(literal)") }
            return .double(value)
        }
    }
}
