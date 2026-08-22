import AuruPM
import AuruPMKernelFFI
import Foundation

/// The compute half: what the pure Swift client deliberately lacks.
///
/// `AuruPM` on its own speaks the protocol, derives commit ids, and verifies
/// hashes — everything needed to browse history and publish. It cannot read a
/// DAW project file, because that is twenty-odd thousand lines of format work
/// and a second implementation would drift from the first.
///
/// This package links that work as a native library instead of reimplementing
/// it, so a phone can answer "what changed between these two versions" with the
/// same code the desktop runs.
///
/// Every function here is a thin call across a C ABI. Buffers returned by that
/// ABI are released before this layer returns, so nothing below needs to be
/// managed by a caller.
public enum Kernel {

    /// The wire protocol version this kernel was built for.
    ///
    /// Worth asserting against ``auruProtocolVersion`` at startup: a binary
    /// artifact and a source package can drift out of step in a way neither
    /// notices otherwise.
    public static var protocolVersion: String {
        (try? call { auru_pm_protocol_version() }).map { String(decoding: $0, as: UTF8.self) } ?? ""
    }

    /// Whether this kernel and the pure client agree on the protocol version.
    public static var matchesClientProtocol: Bool { protocolVersion == auruProtocolVersion }

    // MARK: - Commit identity

    /// Derive a commit's id from its content, using the kernel's canonical rule.
    ///
    /// The pure client derives the same id from the same commit — these are two
    /// implementations of one specification, checked against the same vectors.
    /// This exists so a caller holding a kernel can avoid loading the Swift path
    /// as well, not because the answers differ.
    public static func commitID(fromJSON json: String) throws -> ContentHash {
        try ContentHash(parsing: String(decoding: try call(json) { auru_pm_commit_id($0, $1) },
                                        as: UTF8.self))
    }

    /// The exact bytes a commit's id is the BLAKE3 of.
    public static func canonicalEncoding(fromJSON json: String) throws -> Data {
        Data(try call(json) { auru_pm_commit_canonical_encoding($0, $1) })
    }

    /// BLAKE3 of `bytes`.
    public static func contentHash(of bytes: Data) throws -> ContentHash {
        try ContentHash(parsing: String(decoding: try call(bytes) { auru_pm_content_hash($0, $1) },
                                        as: UTF8.self))
    }

    // MARK: - Reading a project

    /// Identify a project file from its name and leading bytes.
    ///
    /// Returns the wire value — `ableton-live-set`, `fl-studio`, `dawproject`,
    /// `auru`, `bitwig-project`.
    public static func detectFormat(fileName: String, source: Data) throws -> String {
        let name = Array(fileName.utf8)
        let bytes = [UInt8](source)
        return String(
            decoding: try call {
                name.withUnsafeBufferPointer { namePtr in
                    bytes.withUnsafeBufferPointer { bytesPtr in
                        auru_pm_detect_format(
                            namePtr.baseAddress, UInt(namePtr.count),
                            bytesPtr.baseAddress, UInt(bytesPtr.count))
                    }
                }
            }, as: UTF8.self)
    }

    /// Normalize a project file into canonical snapshot bytes.
    ///
    /// This is the call a phone cannot make without the kernel.
    public static func snapshot(fromSource source: Data, format: String) throws -> Data {
        let formatBytes = Array(format.utf8)
        let sourceBytes = [UInt8](source)
        return Data(
            try call {
                formatBytes.withUnsafeBufferPointer { formatPtr in
                    sourceBytes.withUnsafeBufferPointer { sourcePtr in
                        auru_pm_snapshot_from_source(
                            formatPtr.baseAddress, UInt(formatPtr.count),
                            sourcePtr.baseAddress, UInt(sourcePtr.count))
                    }
                }
            })
    }

    /// Rebuild the original project file from canonical snapshot bytes.
    public static func restore(fromSnapshot canonical: Data) throws -> Data {
        Data(try call(canonical) { auru_pm_restore_from_snapshot($0, $1) })
    }

    /// The few-kilobyte summary of what a snapshot is.
    ///
    /// `nil` when the snapshot is of a format this build cannot summarize; the
    /// caller then falls back to reading the snapshot itself.
    public static func projectInfo(fromSnapshot snapshot: Data) throws -> ProjectInfo? {
        let bytes = try call(snapshot) { auru_pm_project_info_from_snapshot($0, $1) }
        guard !bytes.isEmpty else { return nil }
        return try ProjectInfo(parsing: Data(bytes))
    }

    // MARK: - Comparing versions

    /// Per-channel structured diff between two canonical snapshots.
    ///
    /// The reason this package exists: a history screen that shows what changed
    /// rather than only that something did.
    public static func diff(before: Data, after: Data) throws -> JSON {
        try JSON.parse(Data(try call(before, after) { auru_pm_diff_snapshots($0, $1, $2, $3) }))
    }

    /// One line per change, between two canonical snapshots.
    public static func summarize(before: Data, after: Data) throws -> [String] {
        let json = try JSON.parse(
            Data(try call(before, after) { auru_pm_summarize_snapshots($0, $1, $2, $3) }))
        return json.arrayValue.compactMap(\.stringValue)
    }

    /// Three-way merge of canonical snapshots.
    ///
    /// The result is a tagged union: `{"outcome":"clean","merged":…}` or
    /// `{"outcome":"conflict","base":…,"conflicts":[…]}`.
    public static func merge(ancestor: Data, local: Data, remote: Data) throws -> JSON {
        let ancestorBytes = [UInt8](ancestor)
        let localBytes = [UInt8](local)
        let remoteBytes = [UInt8](remote)
        return try JSON.parse(
            Data(
                try call {
                    ancestorBytes.withUnsafeBufferPointer { a in
                        localBytes.withUnsafeBufferPointer { l in
                            remoteBytes.withUnsafeBufferPointer { r in
                                auru_pm_merge_snapshots(
                                    a.baseAddress, UInt(a.count), l.baseAddress, UInt(l.count),
                                    r.baseAddress, UInt(r.count))
                            }
                        }
                    }
                }))
    }

    /// Whether `bytes` hash to `expected`.
    public static func verifyBlob(_ bytes: Data, matches expected: ContentHash) throws -> Bool {
        let payload = [UInt8](bytes)
        let hash = Array(expected.description.utf8)
        let outcome = payload.withUnsafeBufferPointer { payloadPtr in
            hash.withUnsafeBufferPointer { hashPtr in
                auru_pm_verify_blob(
                    payloadPtr.baseAddress, UInt(payloadPtr.count),
                    hashPtr.baseAddress, UInt(hashPtr.count))
            }
        }
        guard outcome >= 0 else {
            throw AuruError(code: .badRequest, message: "not a content hash: \(expected)")
        }
        return outcome == 1
    }

    // MARK: - Crossing the boundary

    /// Run one C call and take ownership of its buffer.
    ///
    /// The buffer is released on every path, including the throwing one — the
    /// ABI's contract is that the caller frees exactly once, and "exactly once"
    /// includes the failure case.
    private static func call(_ body: () -> AuruBuffer) throws -> [UInt8] {
        let buffer = body()
        defer { auru_pm_buffer_free(buffer) }

        let bytes: [UInt8]
        if buffer.len == 0 || buffer.data == nil {
            bytes = []
        } else {
            bytes = Array(UnsafeBufferPointer(start: buffer.data, count: Int(buffer.len)))
        }

        guard buffer.error == 0 else {
            throw AuruError(code: .badRequest, message: String(decoding: bytes, as: UTF8.self))
        }
        return bytes
    }

    private static func call(
        _ input: Data, _ body: (UnsafePointer<UInt8>?, UInt) -> AuruBuffer
    ) throws -> [UInt8] {
        let bytes = [UInt8](input)
        return try call {
            bytes.withUnsafeBufferPointer { body($0.baseAddress, UInt($0.count)) }
        }
    }

    private static func call(
        _ input: String, _ body: (UnsafePointer<UInt8>?, UInt) -> AuruBuffer
    ) throws -> [UInt8] {
        let bytes = Array(input.utf8)
        return try call {
            bytes.withUnsafeBufferPointer { body($0.baseAddress, UInt($0.count)) }
        }
    }

    private static func call(
        _ first: Data, _ second: Data,
        _ body: (UnsafePointer<UInt8>?, UInt, UnsafePointer<UInt8>?, UInt) -> AuruBuffer
    ) throws -> [UInt8] {
        let firstBytes = [UInt8](first)
        let secondBytes = [UInt8](second)
        return try call {
            firstBytes.withUnsafeBufferPointer { a in
                secondBytes.withUnsafeBufferPointer { b in
                    body(a.baseAddress, UInt(a.count), b.baseAddress, UInt(b.count))
                }
            }
        }
    }
}
