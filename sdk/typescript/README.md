# `@auru/pm`

A client for `auru-pm-v1` project-management providers.

The endpoint is always yours to supply. Anyone can run a provider, so there is
no default host here and no registry lookup — which one to trust is the user's
decision, not this library's.

```sh
npm install @auru/pm
```

```ts
import { AuruClient, loadKernel } from "@auru/pm";

const kernel = await loadKernel();
const client = await AuruClient.connect({
  endpoint: "https://pm.example.com",
  accessToken,
  kernel,
});

const project = client.project("user/night-drive");
for (const version of await project.history({ limit: 20 })) {
  console.log(new Date(version.timestamp * 1000), version.message);
}
```

## Two halves

**Transport is TypeScript.** Every byte that crosses the network goes through
`fetch` here — CORS, tokens, interceptors, retries. Nothing in the Rust layer
opens a socket.

**Compute is Rust**, compiled to WebAssembly and to a native N-API addon.
Commit ids, project-format normalization, diff and merge all come from one
implementation, so a dashboard and a desktop client cannot drift apart.

`loadKernel()` picks a binding: the native addon in Node when one is installed,
WebAssembly everywhere else. That is a performance decision, never a correctness
one — the test suite loads both and asserts they produce identical bytes, and
both are checked against `spec/vectors/commit-encoding.json` on every CI run.

WebAssembly is bundled, so the package works the moment it is installed. Native
addons arrive as optional per-platform packages that npm filters by `os`, `cpu`
and `libc`; when none matches — an unusual platform, `--ignore-scripts`, a
bundler that dropped the binary — the wasm path is used instead. Nothing
degrades, it is only slower on large snapshots.

In a browser, `loadKernel()` resolves the `.wasm` beside the module. To control
that — a CDN, a bundler asset URL, an already-fetched `Response` — pass it:

```ts
import { loadWasmKernel } from "@auru/pm";

const kernel = await loadWasmKernel(fetch("/assets/auru_pm_ffi_bg.wasm"));
```

A commit's `id` is the BLAKE3 of an RFC 8785 canonicalization of its content.
Providers recompute it and reject a mismatch, so derive it with `kernel.commitId`
rather than assembling one:

```ts
const commit = { ...draft, id: kernel.commitId(draft) };
await project.putCommit(commit);
```

The SDK is checked against `spec/vectors/commit-encoding.json` on every run.

## Reading a project cheaply

A real Live Set snapshot is around 7 MB and roughly a hundred thousand
elements. Every commit also stores a `ProjectInfo` blob of a few kilobytes —
tempo, key, tracks, plugins — and points at it with `Commit.metadata`. A list
view should read that and never touch the snapshot:

```ts
const commit = await project.getCommit(head);
if (commit.metadata) {
  const info = kernel.projectInfoFromBlob(await project.getBlob(commit.metadata));
  render(info);
}
```

`getBlob` verifies the download against its own hash and throws if the provider
returned different bytes. That is not optional — content addressing is only
worth anything if the reader checks. `getBlobUnverified` exists for callers
verifying elsewhere, and you have to ask for it by name.

## Losing a race

`advanceHead` is a compare-and-swap. When someone else got there first it throws
`HeadConflictError`, which carries the provider's actual HEAD so you can rebase
without another round trip:

```ts
import { isHeadConflict } from "@auru/pm";

try {
  await project.advanceHead(knownHead, commit.id);
} catch (error) {
  if (!isHeadConflict(error)) throw error;
  await rebaseOnto(error.current);
}
```

Every provider error is an `AuruError` with a `code` from a closed set, so you
can switch on it exhaustively. `error.retryable` distinguishes a rate limit or
an unreachable identity provider from a bad token — re-prompting a user because
their provider's IdP was briefly down would be wrong.

## Tokens

The access token lives in memory for the lifetime of the client and nowhere
else. This SDK never writes one to `localStorage` or `sessionStorage`: both are
readable by any script that achieves XSS, and a refresh token sitting in one is
a standing account-takeover primitive.

`completeAuthorization` therefore **discards the refresh token** and returns
only a short-lived access token. When it expires, run the flow again.
`completeAuthorizationWithRefresh` is the explicit opt-in for callers that have
a real secret store — an Electron main process writing to the OS keychain —
and must never be called from a renderer or a browser tab.

For a hosted dashboard, the sturdier shape is a backend-for-frontend: the server
holds the session, the browser holds an `HttpOnly; Secure; SameSite` cookie, and
no token reaches JavaScript at all.

Endpoints come from the issuer's own discovery document, never from the
provider's health response, and a document whose `issuer` does not match exactly
is refused.

## Publishing a project for the first time

A handle only exists once its profile is registered — until then every other
endpoint answers `not_found`, blob upload included:

```ts
const project = client.project("user/night-drive");
await project.putProfile({ display_name: "Night Drive", format: "ableton-live-set" });
```

## Development

Building needs a Rust toolchain, the `wasm32-unknown-unknown` target, and
`wasm-bindgen-cli` matching the crate version in `Cargo.lock`.

```sh
npm install
npm test          # builds both bindings, then runs everything
npm run typecheck # vitest does not typecheck; this does
```

The transport tests start a real `auru-pm-server` and push commits through it.
Mocked HTTP would only prove the client agrees with the test's idea of the
protocol.

To build one platform's addon package:

```sh
node scripts/prepare-platform-package.mjs darwin-arm64 --build
```

## Releasing

Tag `sdk-vX.Y.Z`. The release workflow builds every native addon on its own
runner, checks each against the conformance vectors before it goes anywhere
near npm, publishes the platform packages first and then `@auru/pm`, with npm
provenance attached.
