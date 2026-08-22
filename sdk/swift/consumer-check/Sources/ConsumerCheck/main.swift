// What a consumer actually writes, built against the package as a dependency
// with only its public API in scope.
//
// This uses the shipped `URLSessionTransport` rather than a hand-rolled one, so
// it also proves the adapter product works when consumed separately.

import AuruPM
import AuruPMURLSession
import Foundation

let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
    FileHandle.standardError.write(Data("usage: ConsumerCheck <endpoint> <repo-root>\n".utf8))
    exit(2)
}
let endpoint = arguments[1]
let repoRoot = URL(fileURLWithPath: arguments[2])

let client = try await AuruClient.connect(
    endpoint: endpoint, transport: URLSessionTransport())

print("client protocol:  \(auruProtocolVersion)")
let health = await client.health
print("connected to:     \(health.providerID ?? "?") at \(await client.endpoint)")

let identity = try await client.me()
let project = try await client.project("demo/night-drive")
try await project.putProfile(ProjectProfile(displayName: "Night Drive", format: "dawproject"))

let snapshot = try Data(
    contentsOf: repoRoot.appendingPathComponent(
        "crates/auru-pm-kernel/tests/fixtures/interchange/oracle-midi.dawproject"))
let summary = Data(
    #"{"schema":1,"format":"dawproject","dawproject":{"title":"Night Drive"}}"#.utf8)

let snapshotHash = try await project.putBlob(snapshot)
let samplesHash = try await project.putBlob(Data(#"{"entries":[]}"#.utf8))
let summaryHash = try await project.putBlob(summary)

let commit = try Commit(
    tree: TreeRef(snapshot: snapshotHash, samples: samplesHash),
    author: identity.asAuthor,
    timestamp: Int64(Date().timeIntervalSince1970),
    message: "first take",
    auruVersion: "0.1.0",
    formatVersion: 1,
    metadata: summaryHash)

try await project.putCommit(commit)
try await project.advanceHead(from: nil, to: commit.id)
print("published:        \(commit.id)")

// The dashboard read: the summary blob, never the snapshot.
let head = try await project.getCommit(try await project.head()!)
if let info = try await project.projectInfo(for: head) {
    print("ProjectInfo:      \(summary.count) bytes (snapshot is \(snapshot.count))")
    print("title:            \(info.text("title") ?? "?")")
}

let history = try await project.history(limit: 10)
print("history:          \(history.count) version(s)")

// Losing a compare-and-swap.
do {
    try await project.advanceHead(from: nil, to: commit.id)
    FileHandle.standardError.write(Data("ERROR: a stale compare-and-swap was accepted\n".utf8))
    exit(1)
} catch let error as AuruError where error.code == .headConflict {
    let current = error.currentHead?.description ?? "?"
    print("CAS conflict:     provider HEAD is \(current.prefix(20))...")
}

print("")
print("consumer flow complete, using only the package's public API")
