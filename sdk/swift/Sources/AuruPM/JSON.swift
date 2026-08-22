import Foundation

/// One member of a JSON object.
public struct JSONMember: Sendable, Equatable {
    public let key: String
    public let value: JSON

    public init(_ key: String, _ value: JSON) {
        self.key = key
        self.value = value
    }
}

/// A JSON value, and the canonical encoding a commit id is derived from.
///
/// Written here rather than taken from `JSONSerialization` or `Codable` for two
/// reasons. Canonical encoding needs exact control over the output — a
/// serializer's formatting choices would become this library's bug, and it would
/// surface as a provider rejecting a commit rather than as anything that names
/// the cause. And `JSONSerialization` loses the distinction between an integer
/// and a number that merely has no fractional part, which is precisely the
/// distinction RFC 8785 turns on.
public indirect enum JSON: Sendable, Equatable {
    case null
    case bool(Bool)
    /// A number with no fractional part.
    case int(Int64)
    /// A number with a fractional part or an exponent. Readable, not
    /// canonicalizable — see ``canonicalJSON()``.
    case double(Double)
    case string(String)
    case array([JSON])
    /// Members in insertion order. Canonical output sorts them.
    case object([JSONMember])

    /// The largest integer RFC 8785 can represent exactly.
    ///
    /// Its numbers are IEEE-754 binary64, so beyond this two conformant
    /// implementations derive different ids from the same commit.
    public static let maxSafeInteger: Int64 = 9_007_199_254_740_991

    // MARK: - Reading

    public var isNull: Bool { if case .null = self { return true } else { return false } }

    public var stringValue: String? {
        if case .string(let value) = self { return value } else { return nil }
    }

    public var intValue: Int64? {
        if case .int(let value) = self { return value } else { return nil }
    }

    public var doubleValue: Double? {
        switch self {
        case .int(let value): return Double(value)
        case .double(let value): return value
        default: return nil
        }
    }

    public var boolValue: Bool? {
        if case .bool(let value) = self { return value } else { return nil }
    }

    public var arrayValue: [JSON] {
        if case .array(let values) = self { return values } else { return [] }
    }

    public var objectMembers: [JSONMember] {
        if case .object(let members) = self { return members } else { return [] }
    }

    /// A member by name.
    ///
    /// JSON null reads as absent, so a caller never has to distinguish "missing"
    /// from "explicitly null" when neither carries information.
    public subscript(name: String) -> JSON? {
        guard case .object(let members) = self else { return nil }
        guard let found = members.first(where: { $0.key == name })?.value, !found.isNull else {
            return nil
        }
        return found
    }

    /// A required string member.
    public func string(_ name: String) throws -> String {
        guard let member = self[name] else {
            throw AuruError(code: .badRequest, message: "missing required member \"\(name)\"")
        }
        guard let text = member.stringValue else {
            throw AuruError(code: .badRequest, message: "\"\(name)\" is not a string")
        }
        return text
    }

    /// A required whole-number member.
    public func integer(_ name: String) throws -> Int64 {
        guard let member = self[name] else {
            throw AuruError(code: .badRequest, message: "missing required member \"\(name)\"")
        }
        guard let value = member.intValue else {
            throw AuruError(code: .badRequest, message: "\"\(name)\" is not a whole number")
        }
        return value
    }

    /// An optional boolean member, with a fallback.
    public func bool(_ name: String, default fallback: Bool) -> Bool {
        self[name]?.boolValue ?? fallback
    }

    // MARK: - Building

    /// An object with no members.
    ///
    /// Spelled out because `.object([])` is ambiguous between the enum case and
    /// the tuple-building helper below — an empty literal cannot pick one.
    public static let emptyObject = JSON.object([JSONMember]())

    /// Build an object, omitting members whose value is `nil`.
    ///
    /// Absent and present-and-null are different on this wire, so a profile with
    /// no genre must not carry `"genre": null`.
    public static func object(_ members: [(String, JSON?)]) -> JSON {
        .object(members.compactMap { key, value in
            guard let value, !value.isNull else { return nil }
            return JSONMember(key, value)
        })
    }

    // MARK: - Encoding

    /// RFC 8785 canonical JSON — the bytes a commit id is the BLAKE3 of.
    ///
    /// - Throws: when the value contains a non-integral number, or an integer
    ///   outside ±(2^53 − 1). Producing approximate bytes would mean producing a
    ///   commit id no other implementation agrees with, so this fails instead.
    ///   Nothing in a commit is fractional, and every integer in one is far
    ///   inside the bound.
    public func canonicalJSON() throws -> String {
        var out = ""
        try write(into: &out, canonical: true)
        return out
    }

    /// Ordinary JSON text, for a request body.
    ///
    /// Keeps insertion order, which reads better in a log, and can write a
    /// fractional number. Nothing derives a hash from these bytes.
    public func jsonText() -> String {
        var out = ""
        try? write(into: &out, canonical: false)
        return out
    }

    private func write(into out: inout String, canonical: Bool) throws {
        switch self {
        case .null:
            out += "null"
        case .bool(let value):
            out += value ? "true" : "false"
        case .string(let value):
            JSON.writeString(value, into: &out)
        case .int(let value):
            if canonical, value > JSON.maxSafeInteger || value < -JSON.maxSafeInteger {
                throw AuruError(
                    code: .badRequest,
                    message: """
                        integer \(value) is outside +/-(2^53 - 1); RFC 8785 numbers are \
                        IEEE-754 binary64, so no two implementations would agree on it
                        """)
            }
            out += String(value)
        case .double(let value):
            if canonical {
                throw AuruError(
                    code: .badRequest,
                    message: """
                        cannot canonicalize the non-integral number \(value); RFC 8785 requires \
                        ECMAScript number formatting, which this library implements only for \
                        integers because nothing in a commit is fractional
                        """)
            }
            out += String(value)
        case .array(let elements):
            out += "["
            for (index, element) in elements.enumerated() {
                if index > 0 { out += "," }
                try element.write(into: &out, canonical: canonical)
            }
            out += "]"
        case .object(let members):
            out += "{"
            let ordered = canonical ? members.sorted { JSON.utf16Less($0.key, $1.key) } : members
            for (index, member) in ordered.enumerated() {
                if index > 0 { out += "," }
                JSON.writeString(member.key, into: &out)
                out += ":"
                try member.value.write(into: &out, canonical: canonical)
            }
            out += "}"
        }
    }

    /// Compare two keys the way RFC 8785 requires: by UTF-16 code unit.
    ///
    /// Swift's `<` on `String` compares by Unicode canonical equivalence, which
    /// is neither code-point nor code-unit order — so using it here would be
    /// wrong in a way that only shows up for some inputs. Every key this protocol
    /// uses is ASCII, where all three agree, but a canonicalizer that is only
    /// correct for its own inputs is a trap for whoever reuses it next.
    static func utf16Less(_ left: String, _ right: String) -> Bool {
        var leftUnits = left.utf16.makeIterator()
        var rightUnits = right.utf16.makeIterator()
        while true {
            switch (leftUnits.next(), rightUnits.next()) {
            case (nil, nil): return false
            case (nil, _): return true
            case (_, nil): return false
            case (let a?, let b?):
                if a != b { return a < b }
            }
        }
    }

    /// Escape a string the way ECMAScript's `JSON.stringify` does.
    ///
    /// Only what must be escaped: no escaping of `/`, and none of non-ASCII,
    /// which travels as UTF-8. Escaping more would still be valid JSON and still
    /// be the wrong bytes.
    private static func writeString(_ value: String, into out: inout String) {
        out += "\""
        for scalar in value.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\u{08}": out += "\\b"
            case "\u{0C}": out += "\\f"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            default:
                if scalar.value < 0x20 {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        out += "\""
    }
}
