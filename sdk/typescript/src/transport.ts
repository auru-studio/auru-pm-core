/**
 * HTTP client for `auru-pm-v1`.
 *
 * The endpoint is always supplied by the caller. There is no default host and
 * no registry lookup here: anyone can run a provider, and which one to trust is
 * the user's decision, not this library's.
 *
 * TypeScript owns the whole network path — this is the only place in the SDK
 * that opens a connection. The Rust kernel is pure compute, which is what makes
 * CORS, tokens, interceptors and retries ordinary code rather than something
 * fought for through a wasm HTTP stack.
 */

import { AuruError, HeadConflictError, errorFromResponse } from "./errors.js";
import type { KernelApi } from "./kernel/api.js";
import {
  PROTOCOL,
  type AuthenticatedIdentity,
  type Capabilities,
  type Commit,
  type CommitSummary,
  type ContentHash,
  type HealthResponse,
  type HistoryRange,
  type ProjectProfile,
  type ProviderProject,
  type RetentionReport,
  type RetentionRequest,
} from "./protocol.js";

/** The subset of `fetch` this client uses. */
export type FetchLike = (
  input: string,
  init?: RequestInit,
) => Promise<Response>;

export interface ClientOptions {
  /** Provider base URL, e.g. `https://pm.example.com` or `http://127.0.0.1:4242`. */
  endpoint: string;
  /**
   * Bearer token.
   *
   * Held in memory for the lifetime of the client and nowhere else. In a
   * browser this SDK never writes a token to `localStorage` or
   * `sessionStorage`: both are readable by any script that achieves XSS, and a
   * refresh token sitting there is a standing account-takeover primitive.
   */
  accessToken?: string;
  /** Compute binding, needed to verify downloaded blobs. */
  kernel?: KernelApi;
  /** Override for tests, proxies, or a runtime without a global `fetch`. */
  fetch?: FetchLike;
  /** Aborts every request made through this client. */
  signal?: AbortSignal;
}

/** Strip a trailing slash and reject anything that is not an absolute URL. */
function normalizeEndpoint(endpoint: string): string {
  let parsed: URL;
  try {
    parsed = new URL(endpoint);
  } catch (cause) {
    throw new AuruError("bad_request", `endpoint is not a URL: ${endpoint}`, { cause });
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new AuruError(
      "bad_request",
      `endpoint must be http or https, got ${parsed.protocol}`,
    );
  }
  return `${parsed.origin}${parsed.pathname.replace(/\/+$/, "")}`;
}

/**
 * A connected provider.
 *
 * Construct with {@link connect}, which reads `/v1/health` first: the protocol
 * version, the auth methods, and the capability set all have to be known before
 * the first real call, and one round trip up front is cheaper than discovering
 * an unsupported endpoint halfway through a push.
 */
export class AuruClient {
  readonly endpoint: string;
  readonly health: HealthResponse;

  #accessToken: string | undefined;
  readonly #fetch: FetchLike;
  readonly #kernel: KernelApi | undefined;
  readonly #signal: AbortSignal | undefined;

  private constructor(
    endpoint: string,
    health: HealthResponse,
    options: ClientOptions,
  ) {
    this.endpoint = endpoint;
    this.health = health;
    this.#accessToken = options.accessToken;
    this.#kernel = options.kernel;
    this.#signal = options.signal;
    this.#fetch = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  }

  /**
   * Read `/v1/health` and return a client bound to that provider.
   *
   * Refuses a provider whose protocol string differs from this build's. A
   * mismatch means the shapes below are not the shapes it serves, and finding
   * that out at the first commit rather than here would be worse.
   */
  static async connect(options: ClientOptions): Promise<AuruClient> {
    const endpoint = normalizeEndpoint(options.endpoint);
    const doFetch = options.fetch ?? ((input: string, init?: RequestInit) =>
      globalThis.fetch(input, init));

    let response: Response;
    try {
      response = await doFetch(`${endpoint}/v1/health`, {
        headers: { accept: "application/json" },
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
    } catch (cause) {
      throw new AuruError("internal", `cannot reach ${endpoint}: ${cause}`, { cause });
    }

    if (!response.ok) throw await errorFromResponse(response);

    const health = (await response.json()) as HealthResponse;
    if (health.protocol !== PROTOCOL) {
      throw new AuruError(
        "unsupported",
        `${endpoint} speaks ${health.protocol}; this client speaks ${PROTOCOL}`,
      );
    }
    return new AuruClient(endpoint, health, options);
  }

  get capabilities(): Capabilities {
    return this.health.capabilities;
  }

  /** Replace the bearer token, e.g. after a refresh. */
  setAccessToken(token: string | undefined): void {
    this.#accessToken = token;
  }

  /** Whether a token is currently held. Never exposes the token itself. */
  get authenticated(): boolean {
    return this.#accessToken !== undefined;
  }

  /** Identity the provider derived from the current token. */
  async me(): Promise<AuthenticatedIdentity> {
    return this.#json<AuthenticatedIdentity>("GET", "/v1/me");
  }

  /** Projects visible to this account, newest first. */
  async listProjects(): Promise<ProviderProject[]> {
    this.#require("project_listing", "listing projects");
    const body = await this.#json<{ projects: ProviderProject[] }>(
      "GET",
      "/v1/projects",
    );
    return body.projects;
  }

  /** Scope subsequent calls to one project handle. */
  project(handle: string): ProjectClient {
    if (handle.length === 0) {
      throw new AuruError("bad_request", "project handle must not be empty");
    }
    return new ProjectClient(this, handle);
  }

  /** @internal */
  get kernel(): KernelApi | undefined {
    return this.#kernel;
  }

  /**
   * Refuse a call the provider has not advertised, before spending a round trip
   * on a `422` that says the same thing.
   * @internal
   */
  require(capability: keyof Capabilities, action: string): void {
    this.#require(capability, action);
  }

  #require(capability: keyof Capabilities, action: string): void {
    if (this.capabilities[capability] !== true) {
      throw new AuruError(
        "unsupported",
        `${this.endpoint} does not support ${action} (capability ${capability})`,
      );
    }
  }

  /** @internal */
  async request(
    method: string,
    path: string,
    init: { body?: BodyInit; headers?: Record<string, string> } = {},
  ): Promise<Response> {
    const headers: Record<string, string> = { ...init.headers };
    if (this.#accessToken !== undefined) {
      headers["authorization"] = `Bearer ${this.#accessToken}`;
    }

    let response: Response;
    try {
      response = await this.#fetch(`${this.endpoint}${path}`, {
        method,
        headers,
        ...(init.body === undefined ? {} : { body: init.body }),
        ...(this.#signal === undefined ? {} : { signal: this.#signal }),
      });
    } catch (cause) {
      throw new AuruError("internal", `${method} ${path} failed: ${cause}`, { cause });
    }

    if (!response.ok) throw await errorFromResponse(response);
    return response;
  }

  /** @internal */
  async #json<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await this.request(method, path, {
      ...(body === undefined
        ? {}
        : {
            body: JSON.stringify(body),
            headers: { "content-type": "application/json" },
          }),
      headers: {
        accept: "application/json",
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
    });
    return (await response.json()) as T;
  }

  /** @internal */
  json<T>(method: string, path: string, body?: unknown): Promise<T> {
    return this.#json<T>(method, path, body);
  }
}

/** Calls scoped to one project handle. */
export class ProjectClient {
  readonly #client: AuruClient;
  readonly #handle: string;
  readonly #base: string;

  constructor(client: AuruClient, handle: string) {
    this.#client = client;
    this.#handle = handle;
    this.#base = `/v1/projects/${encodeURIComponent(handle)}`;
  }

  get handle(): string {
    return this.#handle;
  }

  /** Register the human-facing metadata an account project list needs. */
  async putProfile(profile: ProjectProfile): Promise<void> {
    this.#client.require("project_listing", "project profiles");
    await this.#client.request("PUT", this.#base, {
      body: JSON.stringify(profile),
      headers: { "content-type": "application/json" },
    });
  }

  /** Current HEAD, or `null` on a project with no commits. */
  async head(): Promise<ContentHash | null> {
    const body = await this.#client.json<{ commit_id: ContentHash | null }>(
      "GET",
      `${this.#base}/head`,
    );
    return body.commit_id;
  }

  /**
   * Compare-and-swap HEAD from `from` to `to`.
   *
   * Pass `from: null` for the initial publish. Throws {@link HeadConflictError}
   * when the provider's HEAD is not `from`; that error carries the actual HEAD,
   * so a caller can rebase without asking again.
   */
  async advanceHead(from: ContentHash | null, to: ContentHash): Promise<void> {
    await this.#client.json<{ result: string }>("POST", `${this.#base}/head`, {
      from,
      to,
    });
  }

  /**
   * Store a commit.
   *
   * The provider recomputes `commit.id` from the canonical encoding and rejects
   * a mismatch — writing a commit you did not compute is an auth-equivalent
   * failure, not a formatting slip. Derive the id with the kernel's `commitId`
   * rather than assembling it by hand.
   *
   * Idempotent: re-posting an existing id succeeds.
   */
  async putCommit(commit: Commit): Promise<ContentHash> {
    const body = await this.#client.json<{ id: ContentHash }>(
      "POST",
      `${this.#base}/commits`,
      commit,
    );
    return body.id;
  }

  async getCommit(id: ContentHash): Promise<Commit> {
    return this.#client.json<Commit>(
      "GET",
      `${this.#base}/commits/${encodeURIComponent(id)}`,
    );
  }

  /** Commit history, newest first. */
  async history(range: HistoryRange = {}): Promise<CommitSummary[]> {
    const query = new URLSearchParams();
    if (range.limit !== undefined) query.set("limit", String(range.limit));
    if (range.before !== undefined) query.set("before", range.before);
    const suffix = query.size > 0 ? `?${query.toString()}` : "";
    const body = await this.#client.json<{ commits: CommitSummary[] }>(
      "GET",
      `${this.#base}/history${suffix}`,
    );
    return body.commits;
  }

  /**
   * Permanently move the oldest visible-history boundary.
   *
   * Irreversible. Keeping every version means not calling this at all; a
   * provider that has removed a version cannot bring it back when a preference
   * changes later.
   */
  async pruneHistory(request: RetentionRequest): Promise<RetentionReport> {
    this.#client.require("history_retention", "history retention");
    return this.#client.json<RetentionReport>(
      "POST",
      `${this.#base}/retention`,
      request,
    );
  }

  /** Which of `hashes` the provider already holds, parallel-indexed. */
  async hasBlobs(hashes: ContentHash[]): Promise<boolean[]> {
    if (hashes.length === 0) return [];
    const body = await this.#client.json<{ present: boolean[] }>(
      "POST",
      `${this.#base}/blobs/has`,
      { hashes },
    );
    return body.present;
  }

  /**
   * Upload a blob under its own hash.
   *
   * The provider verifies the bytes hash to `hash` and rejects a mismatch.
   * Idempotent.
   */
  async putBlob(hash: ContentHash, bytes: Uint8Array): Promise<void> {
    await this.#client.request("PUT", `${this.#base}/blobs/${encodeURIComponent(hash)}`, {
      body: bytes as BodyInit,
      headers: { "content-type": "application/octet-stream" },
    });
  }

  /**
   * Download a blob and verify it hashes to `hash`.
   *
   * Verification is not optional here: content addressing is only worth
   * anything if the reader checks, and a caller who has to remember to do it
   * separately eventually will not. A kernel must therefore be configured on
   * the client — see {@link getBlobUnverified} for the deliberate escape hatch.
   */
  async getBlob(hash: ContentHash): Promise<Uint8Array> {
    const kernel = this.#client.kernel;
    if (kernel === undefined) {
      throw new AuruError(
        "bad_request",
        "getBlob verifies the download and needs a kernel; pass one to connect(), " +
          "or call getBlobUnverified() if you are verifying elsewhere",
      );
    }
    const bytes = await this.getBlobUnverified(hash);
    if (!kernel.verifyBlob(bytes, hash)) {
      throw new AuruError(
        "bad_request",
        `blob ${hash} does not hash to its own name; the provider returned different bytes`,
      );
    }
    return bytes;
  }

  /** Download a blob without checking it. The caller owns verification. */
  async getBlobUnverified(hash: ContentHash): Promise<Uint8Array> {
    const response = await this.#client.request(
      "GET",
      `${this.#base}/blobs/${encodeURIComponent(hash)}`,
      { headers: { accept: "application/octet-stream" } },
    );
    return new Uint8Array(await response.arrayBuffer());
  }
}

export { HeadConflictError };
