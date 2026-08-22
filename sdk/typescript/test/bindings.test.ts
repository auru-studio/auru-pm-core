/**
 * The two bindings, side by side.
 *
 * `loadKernel()` picks one, and the whole design rests on that being a
 * performance decision rather than a correctness one. So this loads both and
 * checks they agree — on the published vectors, and on a real project file.
 * If they ever diverge, a dashboard and a desktop client would compute
 * different commit ids for the same version, and the first symptom would be a
 * provider rejecting a push.
 */

import { describe, expect, it } from "vitest";

import { loadKernel } from "../src/kernel/load.js";
import { loadWasmKernel } from "../src/kernel/wasm.js";
import type { KernelApi } from "../src/kernel/api.js";
import { fixture, kernel as nativeKernel, readVectors } from "./support.js";

const native = nativeKernel();
const wasm: KernelApi = await loadWasmKernel();
const vectors = readVectors();
const decoder = new TextDecoder();
const DAWPROJECT = fixture("interchange/oracle-midi.dawproject");

describe("the wasm binding", () => {
  it("loads and reports the protocol version", () => {
    expect(wasm.protocol).toBe("auru-pm-v1");
  });

  it("reproduces every published vector", () => {
    for (const testCase of vectors.cases) {
      expect(decoder.decode(wasm.commitCanonicalEncoding(testCase.commit))).toBe(
        testCase.canonical,
      );
      expect(wasm.commitId(testCase.commit)).toBe(testCase.id);
    }
  });

  it("is instantiated once however many times it is requested", async () => {
    const [a, b] = await Promise.all([loadWasmKernel(), loadWasmKernel()]);
    expect(a).toBe(b);
  });
});

describe("wasm and native agree", () => {
  it("on every vector's canonical bytes and id", () => {
    for (const testCase of vectors.cases) {
      expect(decoder.decode(wasm.commitCanonicalEncoding(testCase.commit))).toBe(
        decoder.decode(native.commitCanonicalEncoding(testCase.commit)),
      );
      expect(wasm.commitId(testCase.commit)).toBe(native.commitId(testCase.commit));
    }
  });

  it("on normalizing a real project file", () => {
    const wasmSnapshot = wasm.snapshotFromSource("dawproject", DAWPROJECT);
    const nativeSnapshot = native.snapshotFromSource("dawproject", DAWPROJECT);

    // Byte equality, not just equal hashes: a snapshot is what a commit's tree
    // points at, so a difference here would change every derived id.
    expect(wasmSnapshot).toEqual(nativeSnapshot);
    expect(wasm.contentHash(wasmSnapshot)).toBe(native.contentHash(nativeSnapshot));
  });

  it("on what a project is", () => {
    const snapshot = native.snapshotFromSource("dawproject", DAWPROJECT);
    expect(wasm.projectInfoFromSnapshot(snapshot)).toEqual(
      native.projectInfoFromSnapshot(snapshot),
    );
  });

  it("on restoring a project file", () => {
    const snapshot = native.snapshotFromSource("dawproject", DAWPROJECT);
    expect(wasm.restoreFromSnapshot(snapshot)).toEqual(
      native.restoreFromSnapshot(snapshot),
    );
  });

  it("on merge conflicts", () => {
    const encode = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
    const args = [
      encode({ version: 8, tempo: 120 }),
      encode({ version: 8, tempo: 128 }),
      encode({ version: 8, tempo: 140 }),
    ] as const;

    expect(wasm.mergeSnapshots(...args)).toEqual(native.mergeSnapshots(...args));
  });

  it("on rejecting the same inputs", () => {
    expect(() => wasm.snapshotFromSource("logic-pro" as never, DAWPROJECT)).toThrow();
    expect(() => native.snapshotFromSource("logic-pro" as never, DAWPROJECT)).toThrow();
  });
});

describe("choosing a binding", () => {
  it("prefers the native addon in Node", async () => {
    const chosen = await loadKernel();
    expect(chosen.protocol).toBe("auru-pm-v1");
    expect(chosen.commitId(vectors.cases[0]!.commit)).toBe(vectors.cases[0]!.id);
  });

  it("falls back to wasm rather than failing when no addon exists", async () => {
    // A package installed with --ignore-scripts, or an unusual platform. The
    // fallback is not degraded; it computes the same answers.
    const chosen = await loadKernel({ nativePath: "/nonexistent/auru.node" });
    expect(chosen.commitId(vectors.cases[0]!.commit)).toBe(vectors.cases[0]!.id);
  });

  it("uses wasm when asked to, even where native exists", async () => {
    const chosen = await loadKernel({ preferNative: false });
    expect(chosen.protocol).toBe("auru-pm-v1");
  });
});
