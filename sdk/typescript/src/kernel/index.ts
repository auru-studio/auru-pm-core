/**
 * Loading the compute kernel.
 *
 * The binding artifact is platform-specific — a `.wasm` module in a browser, a
 * `.node` addon in Node or Electron — so this module does not import either
 * one. It takes whichever the runtime loaded and adapts it, which keeps the
 * package importable in a bundler that has no idea what a `.node` file is.
 */

export type { KernelApi, KernelBinding } from "./api.js";
export { createKernel } from "./api.js";
export { loadKernel } from "./load.js";
export type { LoadOptions } from "./load.js";
export { loadWasmKernel } from "./wasm.js";
export type { WasmSource } from "./wasm.js";
export type {
  ChangeKind,
  ChangeRow,
  ChangeTag,
  ChannelDiff,
  ChannelKind,
  ConflictedField,
  MergeOutcome,
  ProjectDiff,
  ProjectInfo,
} from "./types.js";
