/** Shared helpers: the kernel under test, and a real provider to talk to. */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createKernel, type KernelApi, type KernelBinding } from "../src/kernel/api.js";

const here = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(here, "../../..");

/**
 * The N-API build.
 *
 * Tests run against the real binding rather than a stub on purpose: a stub
 * would agree with whatever the tests assumed, and the canonical encoding is
 * exactly the thing an assumption must not be allowed to cover.
 */
export function kernel(): KernelApi {
  const require = createRequire(import.meta.url);
  const path = process.env["AURU_PM_NATIVE"] ?? resolve(here, "../native/auru_pm_ffi.node");
  return createKernel(require(path) as KernelBinding);
}

export function readVectors(): {
  rule: string;
  cases: { name: string; commit: Record<string, unknown>; canonical: string; id: string }[];
} {
  return JSON.parse(
    readFileSync(resolve(repoRoot, "spec/vectors/commit-encoding.json"), "utf8"),
  );
}

export function fixture(name: string): Uint8Array {
  return new Uint8Array(
    readFileSync(resolve(repoRoot, "crates/auru-pm-kernel/tests/fixtures", name)),
  );
}

/** A real `auru-pm-server`, running on a free port with a throwaway data dir. */
export interface Provider {
  endpoint: string;
  stop(): void;
}

let built = false;

export async function startProvider(): Promise<Provider> {
  if (!built) {
    execFileSync("cargo", ["build", "-p", "auru-pm-server", "--locked"], {
      cwd: repoRoot,
      stdio: "ignore",
    });
    built = true;
  }

  const dataDir = mkdtempSync(resolve(tmpdir(), "auru-pm-sdk-"));
  const port = 4300 + Math.floor(Math.random() * 500);
  const configPath = resolve(dataDir, "server.toml");
  writeFileSync(
    configPath,
    [
      "version = 1",
      'provider_id = "sdk-test"',
      `listen = "127.0.0.1:${port}"`,
      `data_dir = ${JSON.stringify(resolve(dataDir, "data"))}`,
      "requests_per_minute = 100000",
      'allowed_origins = ["http://localhost:5173"]',
      "",
      "[authentication]",
      'mode = "none"',
    ].join("\n"),
  );

  const child: ChildProcess = spawn(
    resolve(repoRoot, "target/debug/auru-pm-server"),
    ["--config", configPath],
    { cwd: dataDir, stdio: "ignore" },
  );

  const endpoint = `http://127.0.0.1:${port}`;
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      const response = await fetch(`${endpoint}/v1/health`);
      if (response.ok) {
        return {
          endpoint,
          stop() {
            child.kill();
            rmSync(dataDir, { recursive: true, force: true });
          },
        };
      }
    } catch {
      // not listening yet
    }
    await new Promise((done) => setTimeout(done, 50));
  }

  child.kill();
  rmSync(dataDir, { recursive: true, force: true });
  throw new Error(`provider did not start on ${endpoint}`);
}
