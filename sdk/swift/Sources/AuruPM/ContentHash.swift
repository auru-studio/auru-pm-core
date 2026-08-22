import Foundation

/// A BLAKE3 content hash, in the canonical `blake3:<64 lowercase hex>` form.
///
/// Every hash on the wire uses this shape, commit ids included. The type exists
/// so a hash and an arbitrary string cannot be confused at a call site.
public struct ContentHash: Hashable, Sendable, CustomStringConvertible {

    public let bytes: [UInt8]

    private init(bytes: [UInt8]) {
        self.bytes = bytes
    }

    /// The hash of `data`.
    public static func of(_ data: [UInt8]) -> ContentHash {
        ContentHash(bytes: Blake3.hash(data))
    }

    /// The hash of `data`.
    public static func of(_ data: Data) -> ContentHash {
        of([UInt8](data))
    }

    /// The hash of `text`, encoded as UTF-8.
    public static func of(_ text: String) -> ContentHash {
        of([UInt8](text.utf8))
    }

    /// Parse the canonical string form.
    ///
    /// Uppercase hex is rejected rather than normalized: two spellings of one
    /// hash would compare unequal as strings, and something downstream
    /// eventually compares them as strings.
    public init(parsing text: String) throws {
        let prefix = "blake3:"
        guard text.count == prefix.count + 64, text.hasPrefix(prefix) else {
            throw AuruError(
                code: .badRequest,
                message: "not a canonical content hash: \"\(text)\" (expected blake3:<64 lowercase hex>)")
        }

        var bytes = [UInt8]()
        bytes.reserveCapacity(32)
        var digits = Array(text.dropFirst(prefix.count))
        while !digits.isEmpty {
            let pair = String(digits.prefix(2))
            digits.removeFirst(2)
            guard pair.allSatisfy({ $0.isNumber || ("a"..."f").contains($0) }),
                let value = UInt8(pair, radix: 16)
            else {
                throw AuruError(
                    code: .badRequest,
                    message:
                        "not a canonical content hash: \"\(text)\" (expected blake3:<64 lowercase hex>)")
            }
            bytes.append(value)
        }
        self.bytes = bytes
    }

    /// Whether `data` hashes to this. The check a client owes itself on a download.
    public func matches(_ data: [UInt8]) -> Bool {
        Blake3.hash(data) == bytes
    }

    /// Whether `data` hashes to this.
    public func matches(_ data: Data) -> Bool {
        matches([UInt8](data))
    }

    public var description: String {
        "blake3:" + bytes.map { String(format: "%02x", $0) }.joined()
    }
}
