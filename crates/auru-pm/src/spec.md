# `auru-pm-v1` HTTP spec

Wire protocol for Auru project-management providers. Any third party can
implement a provider in any language; the Auru desktop client speaks to
all providers — bundled filesystem, the Auru-hosted reference, custom
user URLs — through this protocol (or, for in-process providers, through
the matching `ProjectProvider` Rust trait).

Status: **draft, M0**. The shapes are frozen for M1 work to build
against; breaking changes between draft and the M3 hosted launch will be
noted in `CHANGELOG.md` and gated by the version string below.

## Versioning

Providers advertise `"protocol": "auru-pm-v1"` from `GET /v1/health`.
The Auru client refuses to talk to a provider whose protocol string
doesn't match its compiled-in [`WIRE_VERSION`](./lib.rs).

## Authentication

All authenticated endpoints accept a bearer token in the
`Authorization: Bearer <token>` header.

`/v1/health` is always public — clients use it to discover which auth
methods a provider supports before prompting the user.

Auth methods (declared in `/v1/health`):

- `oauth_device_code` — OAuth 2.0 device-code flow (RFC 8628). Used by
  the Auru-hosted reference; the client opens a browser to the URL
  returned by the provider and polls until the user authenticates.
- `pat` — opaque personal access token, pasted into the "Add Custom URL"
  dialog. Stored in the OS keychain keyed by `(provider_id, project_id)`.
- `none` — no token required. Local filesystem / trusted intranet.

## Project handles

Every endpoint that targets a project takes a `{handle}` path segment.
Handles are provider-scoped and opaque to the client — e.g. the hosted
provider uses `user/song-name`, a self-hosted server might use a UUID.
The handle, alongside the provider id, is stored in the project's PM sidecar
under `provider_handles`. Keeping it out of the DAW file lets the same rule
work for native and third-party project formats, while allowing the project
and sidecar to move together without changing remote identity.

## Hash format

All hashes on the wire are the canonical `blake3:<64-hex>` string form
(lowercase, no padding). The Rust client uses [`crate::ContentHash`]'s
`Display` / `FromStr` impls; providers in other languages must produce
the same string.

## Errors

JSON body on non-2xx responses:

```json
{ "code": "head_conflict", "message": "HEAD moved since you fetched it" }
```

Codes:

| Code              | HTTP | Maps to                          |
| ----------------- | ---- | -------------------------------- |
| `bad_request`     | 400  | `Error::Other`                   |
| `unauthorized`    | 401  | `Error::Auth`                    |
| `forbidden`       | 403  | `Error::Auth`                    |
| `not_found`       | 404  | `Error::NotFound`                |
| `head_conflict`   | 409  | `Error::HeadConflict`            |
| `unsupported`     | 422  | `Error::Unsupported`             |
| `internal`        | 500  | `Error::Other`                   |

## Endpoints

### `GET /v1/health`

Public. Returns provider metadata.

```json
{
  "protocol": "auru-pm-v1",
  "name": "Auru Cloud",
  "capabilities": {
    "project_listing": true,
    "members": true,
    "permissions": true,
    "branches": false,
    "server_side_merge": false,
    "auth_methods": ["oauth_device_code"]
  }
}
```

### `GET /v1/projects` *(capability: `project_listing`)*

Lists projects visible to the authenticated account, newest first. This is the
recovery entry point on a machine that has no project sidecars yet.

```json
{
  "projects": [
    {
      "handle": "opaque-provider-handle",
      "head": "blake3:...",
      "profile": {
        "display_name": "Night Drive",
        "format": "ableton-live-set"
      },
      "updated_at": 1750000000
    }
  ]
}
```

`profile` may be absent for a project written by a client predating this
endpoint. The client then reads the HEAD snapshot to determine its format and
uses the opaque handle as a fallback display name.

### `PUT /v1/projects/{handle}` *(capability: `project_listing`)*

Idempotently registers the human-facing metadata required by the account
project list. It does not create a commit or move HEAD.

```json
{ "display_name": "Night Drive", "format": "ableton-live-set" }
```

### `GET /v1/projects/{handle}/head`

Returns the project's current HEAD. `null` on a freshly created (empty)
project.

```json
{ "commit_id": "blake3:..." }
```

or

```json
{ "commit_id": null }
```

### `POST /v1/projects/{handle}/head`

Compare-and-swap HEAD. Body:

```json
{ "from": "blake3:..." | null, "to": "blake3:..." }
```

`from: null` is the initial publish (no prior HEAD). Responses:

- `200 OK`, `{ "result": "advanced" }`
- `409 Conflict`, `{ "code": "head_conflict", "current": "blake3:..." | null }`

### `GET /v1/projects/{handle}/commits/{id}`

Returns the full [`Commit`](./commit.rs) JSON. `404` if the commit isn't
in the provider's log.

### `POST /v1/projects/{handle}/commits`

Body: full `Commit` JSON. Provider MUST verify `id` matches the canonical
encoding of the other fields; a mismatch is `400 bad_request`.

Idempotent: re-posting an existing commit (same `id`) is `200 OK`,
not an error.

Response: `{ "id": "blake3:..." }`.

### `GET /v1/projects/{handle}/history`

Query params:

- `limit` — max rows, default provider-chosen, capped at provider-chosen max.
- `before` — `blake3:<hex>` cursor; return commits strictly older than this id.

Response: array of [`CommitSummary`](./commit.rs).

```json
{ "commits": [ { "id": "blake3:...", "parents": [...], "author": {...}, ... } ] }
```

### `POST /v1/blobs/has`

Body: `{ "hashes": ["blake3:...", ...] }`.

Response: `{ "present": [true, false, ...] }` — same order and length as
the input. Used by the push flow to skip uploading blobs the provider
already has.

### `PUT /v1/blobs/{hash}`

Upload a blob. `Content-Length` required; resumable uploads use standard
`Content-Range` semantics. Provider MUST verify the uploaded bytes hash
to the URL's `{hash}` — mismatch is `400 bad_request`.

Idempotent: re-uploading an existing hash is `200 OK`.

### `GET /v1/blobs/{hash}`

Download a blob. Body is the raw bytes; `Content-Type:
application/octet-stream`. `404` if absent. Clients MUST verify the downloaded
bytes hash to `{hash}` before using them.

### `GET /v1/projects/{handle}/members` *(capability: `members`)*

Response: array of [`Member`](./provider.rs).

```json
{ "members": [ { "user_id": "...", "display_name": "...", "email": "..." } ] }
```

Providers without the capability respond `422 unsupported`.

### `GET /v1/projects/{handle}/permissions/{user}` *(capability: `permissions`)*

Response: [`PermSet`](./provider.rs).

```json
{ "can_read": true, "can_write": true, "can_admin": false }
```

### `POST /v1/auth/device/code` *(OAuth device-code auth only)*

Start the RFC 8628 device authorization flow. Body:

```json
{ "client_id": "auru-desktop" }
```

Response:

```json
{
  "device_code": "...",
  "user_code": "ABCD-1234",
  "verification_uri": "https://auth.provider.example/activate",
  "verification_uri_complete": "https://auth.provider.example/activate?code=ABCD-1234",
  "expires_in": 300,
  "interval": 5
}
```

The client displays `user_code` and `verification_uri` to the user (and
optionally opens a browser to `verification_uri_complete`), then polls
`POST /v1/auth/token` every `interval` seconds.

### `POST /v1/auth/token` *(OAuth device-code auth only)*

Poll for the access token after the user has authenticated. Body:

```json
{
  "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
  "device_code": "...",
  "client_id": "auru-desktop"
}
```

Responses (all 200 OK — errors are in the body per RFC 8628):

```json
{ "access_token": "...", "token_type": "bearer" }
```

```json
{ "error": "authorization_pending" }
{ "error": "slow_down" }
{ "error": "expired_token" }
{ "error": "access_denied" }
```

The returned `access_token` is then used as a bearer token on all
project and blob endpoints exactly like a PAT. It is stored in the OS
keychain keyed by `(provider_id, project_id)`.

## Deferred

- Branches endpoints (`GET/POST /v1/projects/{handle}/branches/...`)
  — reserved; will land when branches ship post-v1.
- Server-side merge (`POST /v1/projects/{handle}/merge`) — reserved for
  providers that opt-in to running the field-level merge themselves.
- Live presence — out of scope here; lives in `plan-collab.prompt.md`.
