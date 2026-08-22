/**
 * Wire types for `auru-pm-v1`.
 *
 * These mirror `spec/openapi.yaml` in the repository root, which is normative.
 * They are hand-written rather than generated because the generated output for
 * a surface this small is harder to read than the surface itself, and the
 * conformance vectors — not the type definitions — are what actually keep an
 * implementation honest.
 */

/** Wire protocol version this client speaks. */
export const PROTOCOL = "auru-pm-v1" as const;

/**
 * BLAKE3 in canonical `blake3:<64 lowercase hex>` form.
 *
 * Every hash on the wire uses this shape, commit ids included.
 */
export type ContentHash = string;

export type ProjectFormat =
  | "auru"
  | "dawproject"
  | "ableton-live-set"
  | "fl-studio"
  | "bitwig-project";

export type AuthMethod =
  | "authorization_code_pkce"
  | "oauth_device_code"
  | "pat"
  | "none";

export type OAuthFlow = "authorization_code_pkce" | "device_authorization";

/**
 * What a provider implements.
 *
 * Every field defaults to `false`, which is what makes capabilities safe to
 * add: a provider written before one existed omits it, and the client reads it
 * as absent rather than guessing.
 */
export interface Capabilities {
  project_listing: boolean;
  members: boolean;
  permissions: boolean;
  branches: boolean;
  server_side_merge: boolean;
  compressed_uploads: boolean;
  history_retention: boolean;
  project_scoped_blobs: boolean;
  auth_methods: AuthMethod[];
}

/**
 * One public OAuth client a provider has registered.
 *
 * A desktop app and a browser dashboard cannot share one: an identity
 * provider's redirect allow-list is per client, and a loopback callback and an
 * https single-page callback have nothing in common.
 */
export interface OAuthClient {
  kind: "native" | "browser";
  client_id: string;
  redirect_uri: string;
  flows: OAuthFlow[];
}

/**
 * Public OAuth configuration.
 *
 * Endpoint URLs are deliberately absent — a client discovers them from the
 * exact issuer via OpenID Connect discovery or RFC 8414 metadata, and rejects
 * an issuer mismatch. Duplicated endpoint configuration is a way to be lied to.
 */
export interface OAuthClientConfiguration {
  issuer: string;
  audience: string;
  required_scope: string;
  /** Every registered client. Providers also write the singular fields below. */
  clients?: OAuthClient[];
  /** Native client id. Pre-`clients` form, still published for older clients. */
  client_id?: string;
  /** Native redirect. Pre-`clients` form. */
  redirect_uri?: string;
  /** Native client flows. Pre-`clients` form. */
  flows?: OAuthFlow[];
}

export interface HealthResponse {
  protocol: string;
  provider_id?: string;
  name?: string;
  capabilities: Capabilities;
  /** Absent for unauthenticated and legacy providers. */
  authentication?: OAuthClientConfiguration;
}

export interface AuthenticatedIdentity {
  provider_id: string;
  /** The token subject. The identity key is `(issuer, sub)`, never email. */
  user_id: string;
  display_name: string;
  email?: string;
}

export interface ProjectLocation {
  /**
   * `/`-separated path beneath a user-selected library root — never an absolute
   * machine-specific path.
   */
  relative_path: string;
}

export interface ProjectMetadata {
  /** Comma-separated categories, e.g. `"Drum & Bass, Jungle"`. */
  genre?: string;
  tags?: string[];
}

export interface ProjectProfile {
  display_name: string;
  format: ProjectFormat;
  metadata?: ProjectMetadata;
  location?: ProjectLocation;
}

export interface ProviderProject {
  handle: string;
  head: ContentHash;
  /**
   * Absent for a project written before catalogues existed. The client then
   * reads the HEAD snapshot for its format and falls back to the handle as a
   * display name.
   */
  profile?: ProjectProfile;
  /** Unix epoch seconds of the HEAD commit. */
  updated_at: number;
}

export interface AuthorIdentity {
  display_name: string;
  provider_user_id: string;
  provider_id: string;
  email?: string;
}

export interface TreeRef {
  /** Blob holding the canonical project JSON for this commit. */
  snapshot: ContentHash;
  /**
   * Blob listing the `(path, hash)` pairs the project depends on. Samples are
   * fetched lazily, so this is the cheap "what do I need before playback" probe.
   */
  samples: ContentHash;
}

/**
 * A commit.
 *
 * `parents.length` is the shape: 0 root, 1 normal, 2 merge.
 *
 * `id` is the BLAKE3 of the RFC 8785 canonicalization of every other field.
 * Providers recompute it and treat a mismatch as auth-equivalent, so never
 * construct one by hand — derive it with the kernel's `commitId`.
 *
 * Every integer here must stay inside ±(2^53 − 1); RFC 8785 numbers are
 * IEEE-754 binary64, and beyond that bound two implementations disagree.
 */
export interface Commit {
  id: ContentHash;
  parents: ContentHash[];
  tree: TreeRef;
  author: AuthorIdentity;
  /** Unix epoch seconds. */
  timestamp: number;
  message: string;
  description?: string;
  auru_version: string;
  format_version: number;
  /**
   * Blob holding this commit's ProjectInfo — a few kilobytes of tempo, key,
   * tracks and plugins, so a client can show what a version *is* without
   * fetching the snapshot, which for a real Live Set is around 7 MB.
   */
  metadata?: ContentHash;
}

/** History row. Omits `tree` so listing does not force a tree fetch per row. */
export interface CommitSummary {
  id: ContentHash;
  parents: ContentHash[];
  author: AuthorIdentity;
  timestamp: number;
  message: string;
  description?: string;
}

/**
 * Destructive history policy.
 *
 * There is deliberately no "keep everything" variant: keeping everything means
 * not calling the endpoint. Once a provider has removed a version, changing a
 * client setting cannot bring it back.
 */
export type RetentionRule =
  | { policy: "latest"; count: number }
  | { policy: "since"; timestamp: number };

export interface RetentionRequest {
  rule: RetentionRule;
  /** In-flight work that must survive even when older than the new boundary. */
  protected_commits?: ContentHash[];
  protected_blobs?: ContentHash[];
}

export interface RetentionReport {
  versions_removed: number;
  /**
   * May be zero even when versions were removed: a provider is allowed to keep
   * newly orphaned objects for a grace period.
   */
  objects_removed: number;
  bytes_freed: number;
}

export interface HistoryRange {
  limit?: number;
  /** Cursor — return commits strictly older than this id. */
  before?: ContentHash;
}
