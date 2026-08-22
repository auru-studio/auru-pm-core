/**
 * Choosing a binding.
 *
 * Both do the same work and produce identical bytes — they are wrappers over
 * one Rust implementation — so which one loads is a performance question, never
 * a correctness one.
 */

import type { KernelApi } from "./api.js";
import { loadWasmKernel } from "./wasm.js";

export interface LoadOptions {
  /**
   * Prefer the native N-API addon when one is available.
   *
   * Defaults to true in Node and Electron's main process. The addon is faster
   * on large snapshots; wasm is the fallback everywhere else and the only
   * option in a browser.
   */
  preferNative?: boolean;
  /** Explicit path to the native addon, overriding discovery. */
  nativePath?: string;
}

function isNodeLike(): boolean {
  return (
    typeof globalThis.process !== "undefined" &&
    globalThis.process.versions?.node !== undefined
  );
}

/**
 * Load the compute kernel appropriate to this runtime.
 *
 * Falls back to wasm whenever the native addon is missing or fails to load —
 * a package installed with `--ignore-scripts`, an unusual platform, a
 * bundler that dropped the binary. Failing softly is right here because the
 * fallback is not degraded: it computes the same answers.
 */
export async function loadKernel(options: LoadOptions = {}): Promise<KernelApi> {
  const preferNative = options.preferNative ?? isNodeLike();

  if (preferNative && isNodeLike()) {
    try {
      const { loadNodeKernel } = await import("./node.js");
      return loadNodeKernel(options.nativePath);
    } catch {
      // No addon for this platform. wasm computes the same answers.
    }
  }

  return loadWasmKernel();
}
