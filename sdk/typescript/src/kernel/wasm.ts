/**
 * Loading the WebAssembly binding.
 *
 * This is the portable one: the same module runs in a browser, in Node, in
 * Electron's renderer, in Deno and in Bun. The native addon is faster, but it
 * has to be built for each platform, so wasm is what makes the package work
 * the moment it is installed.
 */

import { createKernel, type KernelApi, type KernelBinding } from "./api.js";

/** What `wasm-bindgen`'s `--target web` output accepts as a module source. */
export type WasmSource =
  | BufferSource
  | WebAssembly.Module
  | Response
  | URL
  | string;

interface WasmModule extends KernelBinding {
  // wasm-bindgen deprecated the positional form; the object form is what
  // current versions expect and what avoids a console warning on every load.
  default(source?: { module_or_path: WasmSource }): Promise<unknown>;
}

let cached: Promise<KernelApi> | undefined;

function isNode(): boolean {
  return (
    typeof globalThis.process !== "undefined" &&
    globalThis.process.versions?.node !== undefined
  );
}

/**
 * Instantiate the wasm kernel.
 *
 * Pass `source` to control where the module comes from — an already-fetched
 * `Response`, a `URL`, or raw bytes. With no argument the module is found
 * beside this file, which is what a bundler and a plain `<script type=module>`
 * both expect.
 *
 * Instantiation is cached: a page that calls this from several components gets
 * one module rather than several megabytes of duplicates.
 */
export async function loadWasmKernel(source?: WasmSource): Promise<KernelApi> {
  if (source === undefined && cached !== undefined) return cached;

  const load = async (): Promise<KernelApi> => {
    const moduleUrl = new URL("../../wasm/auru_pm_ffi.js", import.meta.url);
    const module = (await import(moduleUrl.href)) as WasmModule;

    if (source !== undefined) {
      await module.default({ module_or_path: source });
    } else if (isNode()) {
      // Node cannot `fetch` a `file:` URL, which is what wasm-bindgen would
      // reach for by default, so read the bytes and hand them over.
      const { readFile } = await import("node:fs/promises");
      const wasmUrl = new URL("../../wasm/auru_pm_ffi_bg.wasm", import.meta.url);
      await module.default({ module_or_path: await readFile(wasmUrl) });
    } else {
      await module.default();
    }

    return createKernel(module);
  };

  if (source !== undefined) return load();

  cached = load();
  return cached;
}
