/// BLAKE3, as used for every hash in `auru-pm-v1`.
///
/// Implemented here rather than taken from a dependency because this package has
/// none, and because a hash is the one thing that can be verified completely:
/// `spec/vectors/content-hash.json` covers every length where BLAKE3's structure
/// changes — the 64-byte compression block, the 1024-byte chunk, and each point
/// where the Merkle tree gains a level.
///
/// Only the plain hash is implemented. Keyed hashing and key derivation are part
/// of BLAKE3 but no part of this protocol, and code that is never exercised is
/// code that is never correct.
struct Blake3 {

    private static let blockLength = 64
    private static let chunkLength = 1024

    private static let chunkStart: UInt32 = 1
    private static let chunkEnd: UInt32 = 2
    private static let parent: UInt32 = 4
    private static let root: UInt32 = 8

    private static let iv: [UInt32] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
        0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
    ]

    private static let messagePermutation: [Int] = [
        2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8,
    ]

    // MARK: - The compression function

    private static func g(
        _ state: inout [UInt32], _ a: Int, _ b: Int, _ c: Int, _ d: Int,
        _ mx: UInt32, _ my: UInt32
    ) {
        state[a] = state[a] &+ state[b] &+ mx
        state[d] = (state[d] ^ state[a]).rotatedRight(by: 16)
        state[c] = state[c] &+ state[d]
        state[b] = (state[b] ^ state[c]).rotatedRight(by: 12)
        state[a] = state[a] &+ state[b] &+ my
        state[d] = (state[d] ^ state[a]).rotatedRight(by: 8)
        state[c] = state[c] &+ state[d]
        state[b] = (state[b] ^ state[c]).rotatedRight(by: 7)
    }

    private static func round(_ state: inout [UInt32], _ m: [UInt32]) {
        // Columns.
        g(&state, 0, 4, 8, 12, m[0], m[1])
        g(&state, 1, 5, 9, 13, m[2], m[3])
        g(&state, 2, 6, 10, 14, m[4], m[5])
        g(&state, 3, 7, 11, 15, m[6], m[7])
        // Diagonals.
        g(&state, 0, 5, 10, 15, m[8], m[9])
        g(&state, 1, 6, 11, 12, m[10], m[11])
        g(&state, 2, 7, 8, 13, m[12], m[13])
        g(&state, 3, 4, 9, 14, m[14], m[15])
    }

    private static func compress(
        chainingValue: [UInt32], blockWords: [UInt32], counter: UInt64,
        blockLength: UInt32, flags: UInt32
    ) -> [UInt32] {
        var state: [UInt32] = [
            chainingValue[0], chainingValue[1], chainingValue[2], chainingValue[3],
            chainingValue[4], chainingValue[5], chainingValue[6], chainingValue[7],
            iv[0], iv[1], iv[2], iv[3],
            UInt32(truncatingIfNeeded: counter),
            UInt32(truncatingIfNeeded: counter >> 32),
            blockLength, flags,
        ]

        var block = blockWords
        for index in 0..<7 {
            round(&state, block)
            if index < 6 {
                var permuted = [UInt32](repeating: 0, count: 16)
                for position in 0..<16 {
                    permuted[position] = block[messagePermutation[position]]
                }
                block = permuted
            }
        }

        for index in 0..<8 {
            state[index] ^= state[index + 8]
            state[index + 8] ^= chainingValue[index]
        }
        return state
    }

    private static func words(from block: [UInt8]) -> [UInt32] {
        var out = [UInt32](repeating: 0, count: 16)
        for index in 0..<16 {
            let offset = index * 4
            out[index] =
                UInt32(block[offset])
                | (UInt32(block[offset + 1]) << 8)
                | (UInt32(block[offset + 2]) << 16)
                | (UInt32(block[offset + 3]) << 24)
        }
        return out
    }

    /// A node's compression inputs, kept unevaluated.
    ///
    /// Whether a node is the root is only known once the input ends, and the
    /// root is compressed with an extra flag — so a node's chaining value and
    /// its root output are different results from the same inputs, and both have
    /// to stay reachable.
    private struct Output {
        var inputChainingValue: [UInt32]
        var blockWords: [UInt32]
        var counter: UInt64
        var blockLength: UInt32
        var flags: UInt32

        func chainingValue() -> [UInt32] {
            Array(
                Blake3.compress(
                    chainingValue: inputChainingValue, blockWords: blockWords,
                    counter: counter, blockLength: blockLength, flags: flags
                ).prefix(8))
        }

        func rootBytes() -> [UInt8] {
            let words = Blake3.compress(
                chainingValue: inputChainingValue, blockWords: blockWords, counter: 0,
                blockLength: blockLength, flags: flags | Blake3.root)
            var out = [UInt8]()
            out.reserveCapacity(32)
            for index in 0..<8 {
                let word = words[index]
                out.append(UInt8(truncatingIfNeeded: word))
                out.append(UInt8(truncatingIfNeeded: word >> 8))
                out.append(UInt8(truncatingIfNeeded: word >> 16))
                out.append(UInt8(truncatingIfNeeded: word >> 24))
            }
            return out
        }
    }

    /// One 1024-byte chunk, accumulated a block at a time.
    private struct ChunkState {
        var chainingValue: [UInt32] = Blake3.iv
        var chunkCounter: UInt64
        var block = [UInt8](repeating: 0, count: Blake3.blockLength)
        var blockLength = 0
        var blocksCompressed = 0

        init(counter: UInt64) { chunkCounter = counter }

        var length: Int { Blake3.blockLength * blocksCompressed + blockLength }

        var startFlag: UInt32 { blocksCompressed == 0 ? Blake3.chunkStart : 0 }

        mutating func update(_ input: ArraySlice<UInt8>) {
            var remaining = input
            while !remaining.isEmpty {
                if blockLength == Blake3.blockLength {
                    // A full block is only compressed once more input arrives:
                    // the final block of a chunk carries CHUNK_END, and whether
                    // this is the final one is not yet known.
                    chainingValue = Array(
                        Blake3.compress(
                            chainingValue: chainingValue, blockWords: Blake3.words(from: block),
                            counter: chunkCounter, blockLength: UInt32(Blake3.blockLength),
                            flags: startFlag
                        ).prefix(8))
                    blocksCompressed += 1
                    block = [UInt8](repeating: 0, count: Blake3.blockLength)
                    blockLength = 0
                }

                let take = min(Blake3.blockLength - blockLength, remaining.count)
                for offset in 0..<take {
                    block[blockLength + offset] = remaining[remaining.startIndex + offset]
                }
                blockLength += take
                remaining = remaining.dropFirst(take)
            }
        }

        func output() -> Output {
            Output(
                inputChainingValue: chainingValue,
                blockWords: Blake3.words(from: block),
                counter: chunkCounter,
                blockLength: UInt32(blockLength),
                flags: startFlag | Blake3.chunkEnd)
        }
    }

    private var chunk = ChunkState(counter: 0)
    private var chainValueStack = [[UInt32]]()

    init() {}

    /// Fold a completed chunk into the tree.
    ///
    /// Every trailing zero bit of the chunk count is one subtree that just
    /// became complete, so this merges exactly that many.
    private mutating func addChunkChainingValue(_ value: [UInt32], totalChunks: UInt64) {
        var value = value
        var remaining = totalChunks
        while remaining & 1 == 0 {
            let left = chainValueStack.removeLast()
            let parentNode = Output(
                inputChainingValue: Blake3.iv,
                blockWords: left + value,
                counter: 0,
                blockLength: UInt32(Blake3.blockLength),
                flags: Blake3.parent)
            value = parentNode.chainingValue()
            remaining >>= 1
        }
        chainValueStack.append(value)
    }

    mutating func update(_ input: [UInt8]) {
        var remaining = input[...]
        while !remaining.isEmpty {
            if chunk.length == Blake3.chunkLength {
                let totalChunks = chunk.chunkCounter + 1
                addChunkChainingValue(chunk.output().chainingValue(), totalChunks: totalChunks)
                chunk = ChunkState(counter: totalChunks)
            }
            let take = min(Blake3.chunkLength - chunk.length, remaining.count)
            chunk.update(remaining.prefix(take))
            remaining = remaining.dropFirst(take)
        }
    }

    func finish() -> [UInt8] {
        var output = chunk.output()
        for left in chainValueStack.reversed() {
            output = Output(
                inputChainingValue: Blake3.iv,
                blockWords: left + output.chainingValue(),
                counter: 0,
                blockLength: UInt32(Blake3.blockLength),
                flags: Blake3.parent)
        }
        return output.rootBytes()
    }

    /// The 32-byte hash of `input`.
    static func hash(_ input: [UInt8]) -> [UInt8] {
        var hasher = Blake3()
        hasher.update(input)
        return hasher.finish()
    }
}

extension UInt32 {
    fileprivate func rotatedRight(by bits: UInt32) -> UInt32 {
        (self >> bits) | (self << (32 - bits))
    }
}
