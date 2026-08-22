#!/usr/bin/env node
//
// Build the N-API addon and put it where the tests expect it.
//
// Cargo names a cdylib per platform, and Node only loads a file called
// `.node`, so this copies rather than symlinks — a symlink would break the
// moment the artifact is rebuilt underneath it.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../..");
const outDir = resolve(here, "../native");

const artifact = {
  darwin: "libauru_pm_ffi.dylib",
  linux: "libauru_pm_ffi.so",
  win32: "auru_pm_ffi.dll",
}[process.platform];

if (artifact === undefined) {
  console.error(`no known cdylib name for platform ${process.platform}`);
  process.exit(1);
}

execFileSync(
  "cargo",
  ["build", "-p", "auru-pm-ffi", "--features", "node", "--release", "--locked"],
  { cwd: repoRoot, stdio: "inherit" },
);

mkdirSync(outDir, { recursive: true });
const destination = resolve(outDir, "auru_pm_ffi.node");
copyFileSync(resolve(repoRoot, "target/release", artifact), destination);
console.log(`native kernel: ${destination}`);
