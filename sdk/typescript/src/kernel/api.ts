/**
 * The compute kernel, and the adapter that turns either binding into it.
 *
 * The wasm build and the N-API addon export identical names and identical
 * shapes — bytes in, JSON strings out — so one adapter serves both. That is
 * the point: a dashboard and an Electron client import the same names, and
 * neither binding can drift from the canonical encoding on its own.
 */

import type { ProjectFormat } from "../protocol.js";
import type { MergeOutcome, ProjectDiff, ProjectInfo } from "./types.js";

/**
 * The raw binding, as `wasm-bindgen` and `napi-rs` generate it.
 *
 * Structured results cross as JSON strings. That is deliberate — it is the one
 * representation both binding generators agree on exactly, so neither runtime
 * gets a subtly different object shape.
 */
export interface KernelBinding {
  protocolVersion(): string;
  commitId(commitJson: string): string;
  commitCanonicalEncoding(commitJson: string): Uint8Array;
  contentHash(bytes: Uint8Array): string;
  verifyBlob(bytes: Uint8Array, expected: string): boolean;
  projectInfoFromSnapshot(snapshot: Uint8Array): string | undefined | null;
  projectInfoFromBlob(blob: Uint8Array): string;
  detectFormat(fileName: string, source: Uint8Array): string;
  snapshotFromSource(format: string, source: Uint8Array): Uint8Array;
  restoreFromSnapshot(canonical: Uint8Array): Uint8Array;
  diffSnapshots(before: Uint8Array, after: Uint8Array): string;
  summarizeSnapshots(before: Uint8Array, after: Uint8Array): string;
  mergeSnapshots(
    ancestor: Uint8Array,
    local: Uint8Array,
    remote: Uint8Array,
  ): string;
}

/** The kernel as the rest of the SDK uses it: objects in, objects out. */
export interface KernelApi {
  /** Wire protocol this build speaks. */
  readonly protocol: string;

  /**
   * Derive a commit's id from its content.
   *
   * Providers recompute this and reject a mismatch, so always derive it rather
   * than trusting an `id` that arrived from somewhere else.
   */
  commitId(commit: unknown): string;

  /**
   * The exact bytes whose BLAKE3 is the commit id.
   *
   * Worth having when a provider rejects a commit: comparing these against
   * `spec/vectors/commit-encoding.json` says immediately whether the client or
   * the provider is wrong.
   */
  commitCanonicalEncoding(commit: unknown): Uint8Array;

  /** BLAKE3 of `bytes`, as `blake3:<64 hex>`. */
  contentHash(bytes: Uint8Array): string;

  /** Whether `bytes` hash to `expected`. */
  verifyBlob(bytes: Uint8Array, expected: string): boolean;

  /** Summarize a canonical snapshot. `null` for a format this build cannot read. */
  projectInfoFromSnapshot(snapshot: Uint8Array): ProjectInfo | null;

  /** Parse a stored ProjectInfo blob. */
  projectInfoFromBlob(blob: Uint8Array): ProjectInfo;

  /** Identify a project file from its name and leading bytes. */
  detectFormat(fileName: string, source: Uint8Array): ProjectFormat;

  /** Normalize a project file into canonical snapshot bytes. */
  snapshotFromSource(format: ProjectFormat, source: Uint8Array): Uint8Array;

  /** Rebuild the original project file from canonical snapshot bytes. */
  restoreFromSnapshot(canonical: Uint8Array): Uint8Array;

  /** Per-channel structured diff between two canonical snapshots. */
  diffSnapshots(before: Uint8Array, after: Uint8Array): ProjectDiff;

  /** One line per change, between two canonical snapshots. */
  summarizeSnapshots(before: Uint8Array, after: Uint8Array): string[];

  /** Three-way merge of canonical snapshots. */
  mergeSnapshots(
    ancestor: Uint8Array,
    local: Uint8Array,
    remote: Uint8Array,
  ): MergeOutcome;
}

/**
 * Wrap a raw binding as a {@link KernelApi}.
 *
 * Pass whichever module the runtime loaded — the wasm one in a browser, the
 * N-API addon in Node or Electron. Nothing below this line knows which.
 */
export function createKernel(binding: KernelBinding): KernelApi {
  const parse = <T>(json: string): T => JSON.parse(json) as T;

  return {
    protocol: binding.protocolVersion(),

    commitId: (commit) => binding.commitId(JSON.stringify(commit)),

    commitCanonicalEncoding: (commit) =>
      binding.commitCanonicalEncoding(JSON.stringify(commit)),

    contentHash: (bytes) => binding.contentHash(bytes),

    verifyBlob: (bytes, expected) => binding.verifyBlob(bytes, expected),

    projectInfoFromSnapshot: (snapshot) => {
      // wasm-bindgen returns `undefined` for a Rust `None`; N-API returns
      // `null`. Normalizing here keeps that difference out of caller code.
      const json = binding.projectInfoFromSnapshot(snapshot);
      return json === undefined || json === null ? null : parse<ProjectInfo>(json);
    },

    projectInfoFromBlob: (blob) => parse<ProjectInfo>(binding.projectInfoFromBlob(blob)),

    detectFormat: (fileName, source) =>
      binding.detectFormat(fileName, source) as ProjectFormat,

    snapshotFromSource: (format, source) => binding.snapshotFromSource(format, source),

    restoreFromSnapshot: (canonical) => binding.restoreFromSnapshot(canonical),

    diffSnapshots: (before, after) =>
      parse<ProjectDiff>(binding.diffSnapshots(before, after)),

    summarizeSnapshots: (before, after) =>
      parse<string[]>(binding.summarizeSnapshots(before, after)),

    mergeSnapshots: (ancestor, local, remote) =>
      parse<MergeOutcome>(binding.mergeSnapshots(ancestor, local, remote)),
  };
}
