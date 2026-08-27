/**
 * OAuth 2.0 Authorization Code with PKCE, for providers that advertise it.
 *
 * Two things here are deliberate and worth reading before changing them.
 *
 * **Endpoints come from discovery, never from the provider's health document.**
 * A provider publishes only its issuer; this module fetches that issuer's RFC
 * 8414 or OpenID Connect metadata and rejects a document whose `issuer` is not
 * byte-identical to the one asked for. A provider that could name its own token
 * endpoint could name someone else's.
 *
 * **No refresh token ever reaches browser storage.** A browser has no secure
 * token store: `localStorage` and `sessionStorage` are readable by any script
 * that achieves XSS, and a refresh token sitting in one is a standing
 * account-takeover primitive. {@link completeAuthorization} therefore discards
 * the refresh token and returns only a short-lived access token, held in
 * memory. Where a real secret store exists — an Electron main process with the
 * OS keychain — {@link completeAuthorizationWithRefresh} is the explicit
 * opt-in.
 */

import { AuruError } from "./errors.js";
import type { FetchLike } from "./transport.js";
import type { OAuthClient, OAuthClientConfiguration } from "./protocol.js";

/** The subset of authorization-server metadata this flow needs. */
export interface AuthorizationServerMetadata {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  code_challenge_methods_supported?: string[];
}

/** An access token, held in memory for as long as the caller keeps it. */
export interface AccessToken {
  accessToken: string;
  /** Seconds from issuance, when the provider said. */
  expiresIn?: number;
  scope?: string;
  tokenType: string;
}

/** An access token plus the refresh token, for callers with a secure store. */
export interface RefreshableToken extends AccessToken {
  refreshToken?: string;
}

/** A prepared authorization request. */
export interface AuthorizationRequest {
  /** Send the user here. */
  url: string;
  /** Echoed back on the redirect; a mismatch means the response is not ours. */
  state: string;
  /**
   * The PKCE secret. Keep it in memory until the redirect returns, and never
   * put it anywhere a different origin or a later session could read.
   */
  codeVerifier: string;
}

function defaultFetch(input: string, init?: RequestInit): Promise<Response> {
  return globalThis.fetch(input, init);
}

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function randomUrlSafe(byteLength: number): string {
  const bytes = new Uint8Array(byteLength);
  globalThis.crypto.getRandomValues(bytes);
  return base64Url(bytes);
}

async function s256(verifier: string): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(verifier),
  );
  return base64Url(new Uint8Array(digest));
}

/**
 * Pick the registered client of a given kind.
 *
 * Falls back to the pre-`clients` singular fields, which describe the native
 * client, so a provider written before the list existed still works.
 */
export function selectClient(
  configuration: OAuthClientConfiguration,
  kind: "native" | "browser" | "mobile",
): OAuthClient {
  const listed = configuration.clients?.find((client) => client.kind === kind);
  if (listed !== undefined) return listed;

  if (
    kind === "native" &&
    configuration.client_id !== undefined &&
    configuration.redirect_uri !== undefined
  ) {
    return {
      kind: "native",
      client_id: configuration.client_id,
      redirect_uri: configuration.redirect_uri,
      flows: configuration.flows ?? ["authorization_code_pkce"],
    };
  }

  throw new AuruError(
    "unsupported",
    `this provider has not registered a ${kind} OAuth client`,
  );
}

/**
 * Fetch and validate an issuer's authorization-server metadata.
 *
 * Tries OpenID Connect discovery, then RFC 8414. The returned `issuer` must
 * equal the one requested exactly; anything else is a redirect to somewhere the
 * user did not choose.
 */
export async function discover(
  issuer: string,
  fetchImpl: FetchLike = defaultFetch,
): Promise<AuthorizationServerMetadata> {
  const base = issuer.replace(/\/+$/, "");
  const candidates = [
    `${base}/.well-known/openid-configuration`,
    `${base}/.well-known/oauth-authorization-server`,
  ];

  let lastError = "no discovery document";
  for (const url of candidates) {
    let response: Response;
    try {
      response = await fetchImpl(url, { headers: { accept: "application/json" } });
    } catch (cause) {
      lastError = `${url}: ${cause}`;
      continue;
    }
    if (!response.ok) {
      lastError = `${url}: ${response.status}`;
      continue;
    }

    const metadata = (await response.json()) as AuthorizationServerMetadata;
    if (metadata.issuer !== issuer) {
      throw new AuruError(
        "unauthorized",
        `discovery at ${url} claims issuer ${metadata.issuer}, expected ${issuer}`,
      );
    }
    if (
      typeof metadata.authorization_endpoint !== "string" ||
      typeof metadata.token_endpoint !== "string"
    ) {
      throw new AuruError("unsupported", `${url} omits the endpoints PKCE needs`);
    }
    return metadata;
  }

  throw new AuruError("unauthorized", `cannot discover ${issuer}: ${lastError}`);
}

/**
 * Build an authorization URL and the PKCE secret that completes it.
 *
 * Refuses a provider whose metadata does not advertise `S256`. Plain challenges
 * are not a fallback worth having: the whole point of PKCE is that an
 * intercepted authorization code is useless, which a plain challenge does not
 * give you.
 */
export async function beginAuthorization(options: {
  metadata: AuthorizationServerMetadata;
  client: OAuthClient;
  scope: string;
  /** Extra parameters, e.g. `prompt` or `login_hint`. */
  extraParams?: Record<string, string>;
}): Promise<AuthorizationRequest> {
  const methods = options.metadata.code_challenge_methods_supported;
  if (methods !== undefined && !methods.includes("S256")) {
    throw new AuruError(
      "unsupported",
      `${options.metadata.issuer} does not support PKCE S256`,
    );
  }
  if (!options.client.flows.includes("authorization_code_pkce")) {
    throw new AuruError(
      "unsupported",
      `client ${options.client.client_id} is not registered for authorization_code_pkce`,
    );
  }

  const codeVerifier = randomUrlSafe(32);
  const state = randomUrlSafe(16);

  const url = new URL(options.metadata.authorization_endpoint);
  url.searchParams.set("response_type", "code");
  url.searchParams.set("client_id", options.client.client_id);
  url.searchParams.set("redirect_uri", options.client.redirect_uri);
  url.searchParams.set("scope", options.scope);
  url.searchParams.set("state", state);
  url.searchParams.set("code_challenge", await s256(codeVerifier));
  url.searchParams.set("code_challenge_method", "S256");
  for (const [key, value] of Object.entries(options.extraParams ?? {})) {
    url.searchParams.set(key, value);
  }

  return { url: url.toString(), state, codeVerifier };
}

async function exchange(options: {
  metadata: AuthorizationServerMetadata;
  client: OAuthClient;
  request: AuthorizationRequest;
  code: string;
  returnedState: string;
  fetch?: FetchLike;
}): Promise<RefreshableToken> {
  if (options.returnedState !== options.request.state) {
    throw new AuruError(
      "unauthorized",
      "authorization state does not match the request; the response is not ours",
    );
  }

  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code: options.code,
    redirect_uri: options.client.redirect_uri,
    client_id: options.client.client_id,
    code_verifier: options.request.codeVerifier,
  });

  const fetchImpl = options.fetch ?? defaultFetch;
  const response = await fetchImpl(options.metadata.token_endpoint, {
    method: "POST",
    headers: {
      "content-type": "application/x-www-form-urlencoded",
      accept: "application/json",
    },
    body: body.toString(),
  });

  const payload = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (!response.ok) {
    const detail =
      typeof payload["error_description"] === "string"
        ? payload["error_description"]
        : typeof payload["error"] === "string"
          ? payload["error"]
          : `${response.status}`;
    throw new AuruError("unauthorized", `token exchange failed: ${detail}`);
  }

  const accessToken = payload["access_token"];
  if (typeof accessToken !== "string") {
    throw new AuruError("unauthorized", "token response carried no access_token");
  }

  return {
    accessToken,
    tokenType: typeof payload["token_type"] === "string" ? payload["token_type"] : "bearer",
    ...(typeof payload["expires_in"] === "number"
      ? { expiresIn: payload["expires_in"] }
      : {}),
    ...(typeof payload["scope"] === "string" ? { scope: payload["scope"] } : {}),
    ...(typeof payload["refresh_token"] === "string"
      ? { refreshToken: payload["refresh_token"] }
      : {}),
  };
}

/**
 * Exchange an authorization code for an access token.
 *
 * The refresh token, if the provider issued one, is discarded here rather than
 * returned. That is the safe default for a browser: there is nowhere to put it
 * that an XSS cannot read, and a caller who is handed one will eventually store
 * it. When the access token expires, run the flow again.
 */
export async function completeAuthorization(options: {
  metadata: AuthorizationServerMetadata;
  client: OAuthClient;
  request: AuthorizationRequest;
  code: string;
  returnedState: string;
  fetch?: FetchLike;
}): Promise<AccessToken> {
  const { refreshToken: _discarded, ...token } = await exchange(options);
  return token;
}

/**
 * Exchange an authorization code, keeping the refresh token.
 *
 * Only for callers with a real secret store — an Electron or Node main process
 * writing to the OS keychain. Never call this from code that runs in a renderer
 * or a browser tab.
 */
export async function completeAuthorizationWithRefresh(options: {
  metadata: AuthorizationServerMetadata;
  client: OAuthClient;
  request: AuthorizationRequest;
  code: string;
  returnedState: string;
  fetch?: FetchLike;
}): Promise<RefreshableToken> {
  return exchange(options);
}
