/**
 * The conformance gate.
 *
 * A commit whose id this SDK cannot reproduce is rejected by every provider, so
 * these are not stylistic assertions — they are the contract. The cases are the
 * ones published in `spec/vectors/`, generated from the Rust implementation.
 */

import { describe, expect, it } from "vitest";

import { kernel, readVectors } from "./support.js";

const pm = kernel();
const vectors = readVectors();
const decoder = new TextDecoder();

describe("canonical commit encoding", () => {
  it("publishes the rule it is checked against", () => {
    expect(vectors.rule).toContain("RFC 8785");
    expect(vectors.cases.length).toBeGreaterThan(10);
  });

  it.each(vectors.cases.map((c) => [c.name, c] as const))(
    "reproduces the canonical bytes for %s",
    (_name, testCase) => {
      const canonical = decoder.decode(pm.commitCanonicalEncoding(testCase.commit));
      expect(canonical).toBe(testCase.canonical);
    },
  );

  it.each(vectors.cases.map((c) => [c.name, c] as const))(
    "derives the recorded id for %s",
    (_name, testCase) => {
      expect(pm.commitId(testCase.commit)).toBe(testCase.id);
    },
  );

  it("ignores the id already on a commit", () => {
    const [first] = vectors.cases;
    if (first === undefined) throw new Error("no vectors");

    const tampered = { ...first.commit, id: "blake3:" + "0".repeat(64) };
    expect(pm.commitId(tampered)).toBe(first.id);
  });

  it("changes the id when any content changes", () => {
    const [first] = vectors.cases;
    if (first === undefined) throw new Error("no vectors");

    const edited = { ...first.commit, message: "a different message" };
    expect(pm.commitId(edited)).not.toBe(first.id);
  });

  it("speaks the protocol version the spec names", () => {
    expect(pm.protocol).toBe("auru-pm-v1");
  });
});
