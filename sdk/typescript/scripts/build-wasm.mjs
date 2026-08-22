#!/usr/bin/env node
//
// Build the browser binding and shrink it.
//
// `--target web` rather than `nodejs` or `bundler`: it produces one ESM module
// with an explicit `init(bytes)` step, which is the only shape that works
// unchanged in a browser, in Node, in a bundler, and in Deno. The others each
// assume one of those and break in the rest.

import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../..");
const outDir = resolve(here, "../wasm");

const run = (command, args, options = {}) =>
  execFileSync(command, args, { stdio: "inherit", ...options });

run(
  "cargo",
  [
    "build",
    "-p",
    "auru-pm-ffi",
    "--features",
    "wasm",
    "--target",
    "wasm32-unknown-unknown",
    "--profile",
    "release-wasm",
    "--locked",
  ],
  { cwd: repoRoot },
);

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

run("wasm-bindgen", [
  "--target",
  "web",
  "--out-dir",
  outDir,
  resolve(repoRoot, "target/wasm32-unknown-unknown/release-wasm/auru_pm_ffi.wasm"),
]);

const wasmPath = resolve(outDir, "auru_pm_ffi_bg.wasm");
const before = statSync(wasmPath).size;

// `-Oz` optimizes for size. The kernel is compute over bytes, so what dominates
// is hashing and parsing rather than codegen; trading a little speed for a much
// smaller download is the right way round for a dashboard.
//
// Binaryen comes from the `binaryen` package rather than the `wasm-opt` wrapper:
// that wrapper is pinned to binaryen 112, which predates the WebAssembly
// features LLVM now emits by default and rejects the module outright.
const wasmOpt = resolve(here, "../node_modules/binaryen/bin/wasm-opt");
//
// The feature flags are explicit because binaryen defaults to a conservative
// baseline while LLVM emits current WebAssembly for wasm32; without them the
// module is rejected as invalid rather than optimized.
run(
  wasmOpt,
  [
    "-Oz",
    "--enable-bulk-memory",
    "--enable-bulk-memory-opt",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-mutable-globals",
    "--enable-reference-types",
    "--enable-multivalue",
    wasmPath,
    "-o",
    wasmPath,
  ],
  { cwd: repoRoot },
);

const after = statSync(wasmPath).size;
const saved = Math.round((1 - after / before) * 100);
console.log(
  `wasm: ${(before / 1024).toFixed(0)} KiB -> ${(after / 1024).toFixed(0)} KiB (-${saved}%)`,
);
