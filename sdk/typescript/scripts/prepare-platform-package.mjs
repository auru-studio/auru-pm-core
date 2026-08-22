#!/usr/bin/env node
//
// Assemble the publishable package for one platform.
//
//   node scripts/prepare-platform-package.mjs <platform-id> [--build]
//
// CI runs this on a matrix, one runner per platform, because a native addon
// has to be linked where it will run.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { PLATFORMS, packageName } from "./platforms.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../..");
const sdkRoot = resolve(here, "..");

const id = process.argv[2];
const platform = PLATFORMS.find((entry) => entry.id === id);
if (platform === undefined) {
  console.error(`unknown platform ${id}; expected one of:`);
  for (const entry of PLATFORMS) console.error(`  ${entry.id}`);
  process.exit(2);
}

const { version, license, repository } = JSON.parse(
  readFileSync(resolve(sdkRoot, "package.json"), "utf8"),
);

if (process.argv.includes("--build")) {
  execFileSync(
    "cargo",
    [
      "build",
      "-p",
      "auru-pm-ffi",
      "--features",
      "node",
      "--release",
      "--locked",
      "--target",
      platform.target,
    ],
    { cwd: repoRoot, stdio: "inherit" },
  );
}

const outDir = resolve(sdkRoot, "platforms", platform.id);
mkdirSync(outDir, { recursive: true });

const binary = `auru-pm-${platform.id}.node`;
copyFileSync(
  resolve(repoRoot, "target", platform.target, "release", platform.artifact),
  resolve(outDir, binary),
);

writeFileSync(
  resolve(outDir, "package.json"),
  `${JSON.stringify(
    {
      name: packageName(platform.id),
      version,
      description: `Native compute kernel for @auru/pm on ${platform.id}.`,
      license,
      repository,
      main: binary,
      files: [binary],
      os: [platform.os],
      cpu: [platform.cpu],
      ...(platform.libc === undefined ? {} : { libc: [platform.libc] }),
    },
    null,
    2,
  )}\n`,
);

writeFileSync(
  resolve(outDir, "README.md"),
  [
    `# ${packageName(platform.id)}`,
    "",
    `The native compute kernel for [\`@auru/pm\`](https://www.npmjs.com/package/@auru/pm) on ${platform.id}.`,
    "",
    "Install `@auru/pm` instead; npm selects this automatically. It is optional:",
    "the main package falls back to WebAssembly, which computes the same answers.",
    "",
  ].join("\n"),
);

console.log(`${packageName(platform.id)} prepared in platforms/${platform.id}`);
