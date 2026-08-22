/**
 * End-to-end against a real `auru-pm-server`.
 *
 * Mocked HTTP would only prove the client agrees with the test's idea of the
 * protocol. These tests push real commits through the reference implementation,
 * which is the only way to find out whether the two actually interoperate.
 */

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { AuruClient, type ProjectClient } from "../src/transport.js";
import { HeadConflictError, isAuruError, isHeadConflict } from "../src/errors.js";
import type { Commit } from "../src/protocol.js";
import type { AuthorIdentity } from "../src/protocol.js";
import { fixture, kernel, startProvider, type Provider } from "./support.js";

const pm = kernel();
let provider: Provider;
let client: AuruClient;
let author: AuthorIdentity;

beforeAll(async () => {
  provider = await startProvider();
  client = await AuruClient.connect({ endpoint: provider.endpoint, kernel: pm });

  // A commit's author must match the identity the provider derived from the
  // token, so a client reads it once rather than inventing one.
  const identity = await client.me();
  author = {
    display_name: identity.display_name,
    provider_user_id: identity.user_id,
    provider_id: identity.provider_id,
  };
});

afterAll(() => provider?.stop());

/**
 * Create a project handle.
 *
 * A provider only knows about a handle once its profile has been registered —
 * every other endpoint answers `not_found` until then. See "Creating a project"
 * in the spec.
 */
async function createProject(handle: string, displayName: string): Promise<ProjectClient> {
  const project = client.project(handle);
  await project.putProfile({ display_name: displayName, format: "dawproject" });
  return project;
}

/** Build, upload and record a commit the way a real client would. */
async function publish(
  project: ProjectClient,
  source: Uint8Array,
  message: string,
  parents: string[] = [],
): Promise<Commit> {
  const format = pm.detectFormat("song.dawproject", source);
  const snapshot = pm.snapshotFromSource(format, source);
  const manifest = new TextEncoder().encode(JSON.stringify({ entries: [] }));

  const snapshotHash = pm.contentHash(snapshot);
  const manifestHash = pm.contentHash(manifest);

  // Skip what the provider already holds — the reason `blobs/has` exists.
  const present = await project.hasBlobs([snapshotHash, manifestHash]);
  if (present[0] !== true) await project.putBlob(snapshotHash, snapshot);
  if (present[1] !== true) await project.putBlob(manifestHash, manifest);

  const draft = {
    id: `blake3:${"0".repeat(64)}`,
    parents,
    tree: { snapshot: snapshotHash, samples: manifestHash },
    author,
    timestamp: 1_700_000_000,
    message,
    description: "",
    auru_version: "0.1.0",
    format_version: 1,
  } satisfies Commit;

  // The provider recomputes this and rejects a mismatch, so it has to come
  // from the kernel rather than from anything the client made up.
  const commit: Commit = { ...draft, id: pm.commitId(draft) };
  const stored = await project.putCommit(commit);
  expect(stored).toBe(commit.id);
  return commit;
}

describe("connecting", () => {
  it("reports the provider's protocol and capabilities", () => {
    expect(client.health.protocol).toBe("auru-pm-v1");
    expect(client.health.provider_id).toBe("sdk-test");
    expect(client.capabilities.project_listing).toBe(true);
  });

  it("accepts an endpoint with a trailing slash", async () => {
    const trimmed = await AuruClient.connect({ endpoint: `${provider.endpoint}/` });
    expect(trimmed.endpoint).toBe(provider.endpoint);
  });

  it("refuses an endpoint that is not a URL", async () => {
    await expect(AuruClient.connect({ endpoint: "pm.example.com" })).rejects.toThrow(
      /not a URL/,
    );
  });

  it("refuses a non-http scheme", async () => {
    await expect(AuruClient.connect({ endpoint: "ftp://pm.example.com" })).rejects.toThrow(
      /must be http or https/,
    );
  });

  it("reports an unreachable endpoint without hanging", async () => {
    await expect(
      AuruClient.connect({ endpoint: "http://127.0.0.1:1" }),
    ).rejects.toThrow(/cannot reach/);
  });
});

describe("a project's life cycle", () => {
  it("publishes a first version and reads it back", async () => {
    const project = await createProject("life-cycle", "Life Cycle");
    const source = fixture("interchange/oracle-midi.dawproject");

    expect(await project.head()).toBeNull();

    const commit = await publish(project, source, "first take");
    await project.advanceHead(null, commit.id);
    expect(await project.head()).toBe(commit.id);

    const fetched = await project.getCommit(commit.id);
    expect(fetched.message).toBe("first take");
    // Round-tripping through the provider must not disturb identity.
    expect(pm.commitId(fetched)).toBe(commit.id);

    const history = await project.history();
    expect(history.map((row) => row.message)).toEqual(["first take"]);
  });

  it("stores a profile and lists the project", async () => {
    const project = await createProject("catalogued", "placeholder");
    const commit = await publish(
      project,
      fixture("interchange/oracle-midi.dawproject"),
      "catalogued",
    );
    await project.advanceHead(null, commit.id);
    await project.putProfile({
      display_name: "Night Drive",
      format: "dawproject",
      metadata: { genre: "Drum & Bass, Jungle", tags: ["wip"] },
    });

    const listed = (await client.listProjects()).find((p) => p.handle === "catalogued");
    expect(listed?.profile?.display_name).toBe("Night Drive");
    expect(listed?.profile?.metadata?.tags).toEqual(["wip"]);
  });

  it("pages history newest first", async () => {
    const project = await createProject("paged", "Paged");
    const source = fixture("interchange/oracle-midi.dawproject");

    let parent: string | null = null;
    const ids: string[] = [];
    for (const message of ["one", "two", "three"]) {
      const commit = await publish(
        project,
        source,
        message,
        parent === null ? [] : [parent],
      );
      await project.advanceHead(parent, commit.id);
      parent = commit.id;
      ids.push(commit.id);
    }

    const all = await project.history();
    expect(all.map((row) => row.message)).toEqual(["three", "two", "one"]);

    const firstPage = await project.history({ limit: 2 });
    expect(firstPage).toHaveLength(2);

    const newest = ids[2];
    if (newest === undefined) throw new Error("expected three commits");
    const older = await project.history({ before: newest });
    expect(older.map((row) => row.message)).toEqual(["two", "one"]);
  });
});

describe("compare-and-swap on HEAD", () => {
  it("reports the actual HEAD when the caller's is stale", async () => {
    const project = await createProject("racing", "Racing");
    const source = fixture("interchange/oracle-midi.dawproject");

    const first = await publish(project, source, "first");
    await project.advanceHead(null, first.id);

    const second = await publish(project, source, "second", [first.id]);
    await project.advanceHead(first.id, second.id);

    // A client that still believes HEAD is `first` loses the race, and needs to
    // learn what it lost to without another round trip.
    const stale = await publish(project, source, "stale", [first.id]);
    let conflict: unknown;
    try {
      await project.advanceHead(first.id, stale.id);
    } catch (error) {
      conflict = error;
    }

    expect(isHeadConflict(conflict)).toBe(true);
    if (!isHeadConflict(conflict)) throw new Error("expected a head conflict");
    expect(conflict).toBeInstanceOf(HeadConflictError);
    expect(conflict.current).toBe(second.id);
    expect(conflict.code).toBe("head_conflict");
    expect(conflict.retryable).toBe(false);
  });
});

describe("blobs", () => {
  it("verifies a download against its own name", async () => {
    const project = await createProject("blobs", "Blobs");
    const bytes = new TextEncoder().encode("kick.wav pretending to be audio");
    const hash = pm.contentHash(bytes);

    expect(await project.hasBlobs([hash])).toEqual([false]);
    await project.putBlob(hash, bytes);
    expect(await project.hasBlobs([hash])).toEqual([true]);

    expect(await project.getBlob(hash)).toEqual(bytes);
  });

  it("re-uploading the same blob is not an error", async () => {
    const project = client.project("blobs");
    const bytes = new TextEncoder().encode("idempotent");
    const hash = pm.contentHash(bytes);
    await project.putBlob(hash, bytes);
    await expect(project.putBlob(hash, bytes)).resolves.toBeUndefined();
  });

  it("refuses to fetch a blob unverified when no kernel is configured", async () => {
    const bare = await AuruClient.connect({ endpoint: provider.endpoint });
    const bytes = new TextEncoder().encode("unverifiable");
    const hash = pm.contentHash(bytes);
    await bare.project("blobs").putBlob(hash, bytes);

    await expect(bare.project("blobs").getBlob(hash)).rejects.toThrow(/needs a kernel/);
    // The escape hatch exists, but you have to ask for it by name.
    expect(await bare.project("blobs").getBlobUnverified(hash)).toEqual(bytes);
  });

  it("asking for a blob that does not exist is not_found", async () => {
    const missing = pm.contentHash(new TextEncoder().encode("never uploaded"));
    try {
      await client.project("blobs").getBlob(missing);
      throw new Error("expected a failure");
    } catch (error) {
      expect(isAuruError(error)).toBe(true);
      if (isAuruError(error)) expect(error.code).toBe("not_found");
    }
  });
});

describe("what the provider rejects", () => {
  it("refuses a commit whose id does not match its content", async () => {
    const project = await createProject("tampered", "Tampered");
    const source = fixture("interchange/oracle-midi.dawproject");
    const snapshot = pm.snapshotFromSource("dawproject", source);
    const manifest = new TextEncoder().encode(JSON.stringify({ entries: [] }));
    await project.putBlob(pm.contentHash(snapshot), snapshot);
    await project.putBlob(pm.contentHash(manifest), manifest);

    const commit: Commit = {
      id: `blake3:${"a".repeat(64)}`,
      parents: [],
      tree: { snapshot: pm.contentHash(snapshot), samples: pm.contentHash(manifest) },
      author,
      timestamp: 1_700_000_000,
      message: "an id I did not compute",
      description: "",
      auru_version: "0.1.0",
      format_version: 1,
    };

    try {
      await project.putCommit(commit);
      throw new Error("expected the provider to reject it");
    } catch (error) {
      expect(isAuruError(error)).toBe(true);
      if (isAuruError(error)) expect(error.code).toBe("bad_request");
    }
  });

  it("reports a missing commit as not_found", async () => {
    const absent = `blake3:${"b".repeat(64)}`;
    try {
      await client.project("life-cycle").getCommit(absent);
      throw new Error("expected a failure");
    } catch (error) {
      expect(isAuruError(error)).toBe(true);
      if (isAuruError(error)) expect(error.code).toBe("not_found");
    }
  });

  it("rejects an empty project handle before sending anything", () => {
    expect(() => client.project("")).toThrow(/must not be empty/);
  });
});
