/**
 * The compute surface, through the real binding.
 *
 * The dashboard path is the one that matters most here: read the ProjectInfo
 * blob a commit points at, not the snapshot it summarizes.
 */

import { describe, expect, it } from "vitest";

import { fixture, kernel } from "./support.js";

const pm = kernel();
const DAWPROJECT = fixture("interchange/oracle-midi.dawproject");

describe("reading a project", () => {
  it("identifies a project file from its name and bytes", () => {
    expect(pm.detectFormat("song.dawproject", DAWPROJECT)).toBe("dawproject");
  });

  it("normalizes and restores without changing identity", () => {
    const snapshot = pm.snapshotFromSource("dawproject", DAWPROJECT);
    const restored = pm.restoreFromSnapshot(snapshot);
    const again = pm.snapshotFromSource("dawproject", restored);
    expect(pm.contentHash(again)).toBe(pm.contentHash(snapshot));
  });

  it("summarizes a project far more cheaply than the snapshot it came from", () => {
    const snapshot = pm.snapshotFromSource("dawproject", DAWPROJECT);
    const info = pm.projectInfoFromSnapshot(snapshot);

    expect(info).not.toBeNull();
    if (info === null) return;
    expect(info.format).toBe("dawproject");
    expect(info.dawproject).toBeDefined();

    // The reason a commit stores this separately: a project list can render
    // without ever fetching a snapshot.
    const summarySize = JSON.stringify(info).length;
    expect(summarySize).toBeLessThan(snapshot.length);
  });

  it("returns null rather than throwing for a snapshot it cannot summarize", () => {
    const snapshot = pm.snapshotFromSource("auru", new TextEncoder().encode("{}"));
    expect(pm.projectInfoFromSnapshot(snapshot)).toBeNull();
  });

  it("round-trips a ProjectInfo blob", () => {
    const snapshot = pm.snapshotFromSource("dawproject", DAWPROJECT);
    const info = pm.projectInfoFromSnapshot(snapshot);
    if (info === null) throw new Error("expected a summary");

    const blob = new TextEncoder().encode(JSON.stringify(info));
    expect(pm.projectInfoFromBlob(blob)).toEqual(info);
  });

  it("refuses a format it does not know, by name", () => {
    expect(() => pm.snapshotFromSource("logic-pro" as never, DAWPROJECT)).toThrow(
      /unknown project format/,
    );
  });
});

describe("content addressing", () => {
  it("names bytes in the canonical hash form", () => {
    expect(pm.contentHash(new TextEncoder().encode("audio"))).toMatch(
      /^blake3:[0-9a-f]{64}$/,
    );
  });

  it("accepts the right bytes and rejects any others", () => {
    const bytes = new TextEncoder().encode("kick.wav");
    const hash = pm.contentHash(bytes);
    expect(pm.verifyBlob(bytes, hash)).toBe(true);
    expect(pm.verifyBlob(new TextEncoder().encode("snare.wav"), hash)).toBe(false);
  });

  it("refuses a malformed hash rather than quietly failing the comparison", () => {
    expect(() => pm.verifyBlob(new Uint8Array(), "sha256:deadbeef")).toThrow();
  });
});

describe("comparing versions", () => {
  const encode = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));

  it("reports no structural change between identical snapshots", () => {
    const snapshot = pm.snapshotFromSource("dawproject", DAWPROJECT);
    expect(pm.summarizeSnapshots(snapshot, snapshot)).toEqual(["No structural changes"]);
  });

  it("returns a structured diff a renderer can walk", () => {
    const snapshot = pm.snapshotFromSource("dawproject", DAWPROJECT);
    const diff = pm.diffSnapshots(snapshot, snapshot);
    expect(Array.isArray(diff.project_changes)).toBe(true);
    expect(Array.isArray(diff.channels)).toBe(true);
    expect(diff.time_sig).toHaveLength(2);
  });

  it("merges disjoint edits cleanly", () => {
    const outcome = pm.mergeSnapshots(
      encode({ version: 8, tempo: 120, key: "C" }),
      encode({ version: 8, tempo: 128, key: "C" }),
      encode({ version: 8, tempo: 120, key: "G" }),
    );

    expect(outcome.outcome).toBe("clean");
    if (outcome.outcome !== "clean") return;
    expect(outcome.merged).toMatchObject({ tempo: 128, key: "G" });
  });

  it("names the field when both sides changed it differently", () => {
    const outcome = pm.mergeSnapshots(
      encode({ version: 8, tempo: 120 }),
      encode({ version: 8, tempo: 128 }),
      encode({ version: 8, tempo: 140 }),
    );

    expect(outcome.outcome).toBe("conflict");
    if (outcome.outcome !== "conflict") return;
    expect(outcome.conflicts).toHaveLength(1);
    expect(outcome.conflicts[0]?.path).toBe("tempo");
    expect(outcome.conflicts[0]?.local).toBe(128);
    expect(outcome.conflicts[0]?.remote).toBe(140);
  });
});
