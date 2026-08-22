# `auru-pm-v1`

The contract third parties implement against. Normative — where this directory
and any implementation disagree, this directory is correct.

| File | What it is |
| --- | --- |
| [`auru-pm-v1.md`](./auru-pm-v1.md) | The specification. Prose is normative. |
| [`openapi.yaml`](./openapi.yaml) | OpenAPI 3.1 for the HTTP surface — 13 operations, 27 schemas. |
| [`schemas/commit.schema.json`](./schemas/commit.schema.json) | Standalone JSON Schema for `Commit`. Separate from the OpenAPI because canonical encoding is a hashing contract, not an HTTP one. |
| [`vectors/commit-encoding.json`](./vectors/commit-encoding.json) | Frozen conformance cases: commit in, canonical bytes out, resulting id. |

## Writing an implementation

Two things are easy to get wrong, and both are covered by the vectors rather
than left to prose:

1. **Commit ids.** `id` is the BLAKE3 of the RFC 8785 canonicalization of the
   commit with `id` removed. Providers verify it and treat a mismatch as
   auth-equivalent — a client writing what it did not compute. Use a JCS library
   rather than reimplementing a serializer, and check yourself against the
   vectors before anything else.
2. **Integers.** RFC 8785 numbers are IEEE-754 binary64. Every integer on a
   commit must stay inside ±(2^53 − 1) or two conformant implementations will
   derive different ids from the same commit.

Blob payloads are *not* canonicalized on the wire. They are opaque bytes
addressed by the BLAKE3 of exactly what was uploaded; a reader verifies the hash
of what it received and never re-derives it.

## Keeping it honest

The vectors are generated from, and verified against, the reference
implementation:

```sh
cargo test -p auru-pm --test spec_vectors
```

That test fails if the encoding drifts from the published file. After an
intentional change — which re-derives every existing commit id, so be sure —
regenerate with `UPDATE_SPEC_VECTORS=1`.

```sh
npx @redocly/cli lint spec/openapi.yaml
```
