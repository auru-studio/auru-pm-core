/**
 * Loading the native N-API addon in Node or Electron.
 *
 * Kept out of `kernel/index.ts` so a browser bundle never has to resolve a
 * `.node` file that will not exist there.
 *
 * The addon ships as one small package per platform, listed in this package's
 * `optionalDependencies`. npm installs only the entry matching `os`, `cpu` and
 * `libc`, so nobody downloads five binaries they cannot run. All of them are
 * optional: {@link import("./load.js").loadKernel} falls back to WebAssembly,
 * which computes the same answers.
 */

import { createRequire } from "node:module";

import { createKernel, type KernelApi, type KernelBinding } from "./api.js";

/**
 * Which libc this Node was built against.
 *
 * A glibc binary will not load on Alpine and vice versa, and the failure is a
 * confusing linker error rather than anything that names the cause — so the
 * package id carries it.
 */
function libcSuffix(): string {
  if (process.platform !== "linux") return "";
  const report = process.report?.getReport();
  const glibc =
    typeof report === "object" && report !== null
      ? (report as { header?: { glibcVersionRuntime?: string } }).header
          ?.glibcVersionRuntime
      : undefined;
  return glibc === undefined ? "-musl" : "-gnu";
}

/** The platform id used for both the package name and the binary name. */
export function platformId(): string {
  return `${process.platform}-${process.arch}${libcSuffix()}${
    process.platform === "win32" ? "-msvc" : ""
  }`;
}

/**
 * Load the native addon and adapt it.
 *
 * Tries, in order: an explicit path, the `AURU_PM_NATIVE` override, the
 * published package for this platform, and a local development build.
 */
export function loadNodeKernel(modulePath?: string): KernelApi {
  const require = createRequire(import.meta.url);
  const id = platformId();

  const candidates = [
    modulePath,
    process.env["AURU_PM_NATIVE"],
    `@auru/pm-${id}`,
    new URL(`../../native/auru-pm-${id}.node`, import.meta.url).pathname,
    new URL("../../native/auru_pm_ffi.node", import.meta.url).pathname,
  ].filter((candidate): candidate is string => candidate !== undefined);

  const failures: string[] = [];
  for (const candidate of candidates) {
    try {
      return createKernel(require(candidate) as KernelBinding);
    } catch (error) {
      failures.push(`${candidate}: ${(error as Error).message.split("\n")[0]}`);
    }
  }

  throw new Error(
    `no native kernel for ${id}. Tried:\n  ${failures.join("\n  ")}\n` +
      "This is not fatal — loadKernel() falls back to WebAssembly.",
  );
}
