#!/usr/bin/env node
//
// Check one JavaScript-reachable implementation against the published vectors.
//
//   node spec/conformance/verify-vectors.mjs <module-path>
//
// The module must export `commitCanonicalEncoding(commitJson) -> Uint8Array`
// and `commitId(commitJson) -> string`. Both the wasm build and the N-API
// addon satisfy that, and so should any other binding: the point is that one
// runner proves every implementation reproduces the same bytes.
//
// Exit status is 0 only when every case matches.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const modulePath = process.argv[2];

if (!modulePath) {
  console.error("usage: verify-vectors.mjs <module-path>");
  process.exit(2);
}

const require = createRequire(import.meta.url);
const implementation = require(resolve(process.cwd(), modulePath));

for (const name of ["commitCanonicalEncoding", "commitId"]) {
  if (typeof implementation[name] !== "function") {
    console.error(`${modulePath} does not export ${name}()`);
    process.exit(2);
  }
}

const doc = JSON.parse(
  readFileSync(resolve(here, "../vectors/commit-encoding.json"), "utf8"),
);
const decoder = new TextDecoder();

console.log(`implementation: ${modulePath}`);
console.log(`rule:           ${doc.rule}`);
console.log(`integer bound:  ${doc.integer_bound}\n`);

let failures = 0;

for (const testCase of doc.cases) {
  const commitJson = JSON.stringify(testCase.commit);
  const problems = [];

  try {
    const canonical = decoder.decode(
      implementation.commitCanonicalEncoding(commitJson),
    );
    if (canonical !== testCase.canonical) {
      problems.push(
        `canonical bytes differ\n    expected: ${testCase.canonical}\n    actual:   ${canonical}`,
      );
    }
  } catch (error) {
    problems.push(`commitCanonicalEncoding threw: ${error}`);
  }

  try {
    const id = implementation.commitId(commitJson);
    if (id !== testCase.id) {
      problems.push(`id differs\n    expected: ${testCase.id}\n    actual:   ${id}`);
    }
  } catch (error) {
    problems.push(`commitId threw: ${error}`);
  }

  if (problems.length > 0) {
    failures++;
    console.log(`FAIL  ${testCase.name}`);
    for (const problem of problems) console.log(`  ${problem}`);
  } else {
    console.log(`ok    ${testCase.name}`);
  }
}

const passed = doc.cases.length - failures;
console.log(`\n${passed}/${doc.cases.length} vectors reproduced`);

if (failures > 0) {
  console.log(
    "\nA commit whose id an implementation cannot reproduce is rejected by every\n" +
      "provider, so this is a hard failure rather than a warning.",
  );
}

process.exit(failures === 0 ? 0 : 1);
