/**
 * Provider errors.
 *
 * `code` is a closed set — the table in `spec/auru-pm-v1.md` — so a caller can
 * switch on it exhaustively and TypeScript will say when a case is missing.
 */

import type { ContentHash } from "./protocol.js";

export type ErrorCode =
  | "bad_request"
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "head_conflict"
  | "unsupported"
  | "rate_limited"
  | "storage_error"
  | "internal"
  | "authentication_unavailable";

/**
 * Codes worth retrying without changing anything about the request.
 *
 * `authentication_unavailable` is in here deliberately: the identity provider
 * was unreachable, so the token was never judged. Prompting the user to sign in
 * again would be wrong.
 */
const RETRYABLE: ReadonlySet<ErrorCode> = new Set<ErrorCode>([
  "rate_limited",
  "storage_error",
  "internal",
  "authentication_unavailable",
]);

/** An error returned by a provider, or raised before a request was sent. */
export class AuruError extends Error {
  readonly code: ErrorCode;
  readonly status: number | undefined;
  /** Seconds to wait, from `Retry-After`. Only ever set on `rate_limited`. */
  readonly retryAfter: number | undefined;

  constructor(
    code: ErrorCode,
    message: string,
    options: { status?: number; retryAfter?: number; cause?: unknown } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "AuruError";
    this.code = code;
    this.status = options.status;
    this.retryAfter = options.retryAfter;
  }

  /** Whether retrying the identical request could succeed. */
  get retryable(): boolean {
    return RETRYABLE.has(this.code);
  }
}

/**
 * The `head_conflict` case, which carries the provider's actual HEAD rather
 * than a message.
 *
 * That is the whole point of the shape: a client that lost the compare-and-swap
 * can rebase immediately instead of spending another round trip asking what it
 * lost to.
 */
export class HeadConflictError extends AuruError {
  /** The provider's current HEAD. `null` on a project with no commits yet. */
  readonly current: ContentHash | null;

  constructor(current: ContentHash | null) {
    super("head_conflict", "HEAD moved since it was last read", { status: 409 });
    this.name = "HeadConflictError";
    this.current = current;
  }
}

/** Whether `value` is an `AuruError`, narrowing for a `catch` block. */
export function isAuruError(value: unknown): value is AuruError {
  return value instanceof AuruError;
}

/** Whether `value` is the compare-and-swap failure, narrowed to expose `current`. */
export function isHeadConflict(value: unknown): value is HeadConflictError {
  return value instanceof HeadConflictError;
}

const KNOWN_CODES: ReadonlySet<string> = new Set<ErrorCode>([
  "bad_request",
  "unauthorized",
  "forbidden",
  "not_found",
  "head_conflict",
  "unsupported",
  "rate_limited",
  "storage_error",
  "internal",
  "authentication_unavailable",
]);

/**
 * Turn a non-2xx response into the right error.
 *
 * A provider that invents a code outside the table, or returns something that
 * is not JSON at all, still has to produce a usable error here — a proxy
 * returning an HTML 502 page is the ordinary case, not a hypothetical.
 */
export async function errorFromResponse(response: Response): Promise<AuruError> {
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }

  const record = (body ?? {}) as Record<string, unknown>;
  const rawCode = typeof record["code"] === "string" ? record["code"] : undefined;
  const message =
    typeof record["message"] === "string"
      ? record["message"]
      : `${response.status} ${response.statusText}`;

  if (rawCode === "head_conflict") {
    const current = record["current"];
    return new HeadConflictError(typeof current === "string" ? current : null);
  }

  const code: ErrorCode =
    rawCode !== undefined && KNOWN_CODES.has(rawCode)
      ? (rawCode as ErrorCode)
      : codeForStatus(response.status);

  const retryHeader = response.headers.get("retry-after");
  const retryAfter =
    retryHeader !== null && /^\d+$/.test(retryHeader) ? Number(retryHeader) : undefined;

  return new AuruError(code, message, {
    status: response.status,
    ...(retryAfter === undefined ? {} : { retryAfter }),
  });
}

/** Fallback when a provider sends no usable `code`. */
function codeForStatus(status: number): ErrorCode {
  switch (status) {
    case 400:
      return "bad_request";
    case 401:
      return "unauthorized";
    case 403:
      return "forbidden";
    case 404:
      return "not_found";
    case 409:
      return "head_conflict";
    case 422:
      return "unsupported";
    case 429:
      return "rate_limited";
    case 503:
      return "authentication_unavailable";
    default:
      return status >= 500 ? "internal" : "bad_request";
  }
}
