/**
 * PKCE, discovery, and the token rules.
 *
 * The assertions here are mostly about what the flow *refuses*. An OAuth client
 * that accepts a discovery document naming someone else's token endpoint, or
 * that hands a refresh token to browser code, fails in a way its user will
 * never see until it matters.
 */

import { describe, expect, it } from "vitest";

import {
  beginAuthorization,
  completeAuthorization,
  completeAuthorizationWithRefresh,
  discover,
  selectClient,
} from "../src/auth.js";
import type { FetchLike } from "../src/transport.js";
import type { OAuthClientConfiguration } from "../src/protocol.js";

const ISSUER = "https://identity.example.com";

const METADATA = {
  issuer: ISSUER,
  authorization_endpoint: `${ISSUER}/authorize`,
  token_endpoint: `${ISSUER}/token`,
  code_challenge_methods_supported: ["S256"],
};

const BROWSER_CLIENT = {
  kind: "browser" as const,
  client_id: "dashboard",
  redirect_uri: "https://dashboard.example.com/oauth/callback",
  flows: ["authorization_code_pkce" as const],
};

function jsonFetch(body: unknown, status = 200): FetchLike {
  return async () =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });
}

describe("choosing a client", () => {
  const configuration: OAuthClientConfiguration = {
    issuer: ISSUER,
    audience: "auru-pm",
    required_scope: "openid",
    client_id: "desktop",
    redirect_uri: "http://127.0.0.1:43827/oauth/callback",
    flows: ["authorization_code_pkce"],
    clients: [
      {
        kind: "native",
        client_id: "desktop",
        redirect_uri: "http://127.0.0.1:43827/oauth/callback",
        flows: ["authorization_code_pkce"],
      },
      BROWSER_CLIENT,
    ],
  };

  it("finds each registered kind", () => {
    expect(selectClient(configuration, "browser").client_id).toBe("dashboard");
    expect(selectClient(configuration, "native").client_id).toBe("desktop");
  });

  it("reads the pre-`clients` singular fields as the native client", () => {
    const legacy: OAuthClientConfiguration = {
      issuer: ISSUER,
      audience: "auru-pm",
      required_scope: "openid",
      client_id: "desktop",
      redirect_uri: "http://127.0.0.1:43827/oauth/callback",
      flows: ["authorization_code_pkce"],
    };
    expect(selectClient(legacy, "native").client_id).toBe("desktop");
  });

  it("says so plainly when a provider has no dashboard client", () => {
    const nativeOnly: OAuthClientConfiguration = {
      issuer: ISSUER,
      audience: "auru-pm",
      required_scope: "openid",
      client_id: "desktop",
      redirect_uri: "http://127.0.0.1:43827/oauth/callback",
      flows: ["authorization_code_pkce"],
    };
    expect(() => selectClient(nativeOnly, "browser")).toThrow(/has not registered a browser/);
  });
});

describe("discovery", () => {
  it("accepts a document whose issuer matches exactly", async () => {
    const metadata = await discover(ISSUER, jsonFetch(METADATA));
    expect(metadata.token_endpoint).toBe(`${ISSUER}/token`);
  });

  it("refuses a document claiming a different issuer", async () => {
    // A provider that could name someone else's issuer could send a user's
    // credentials there.
    const lying = { ...METADATA, issuer: "https://attacker.example.com" };
    await expect(discover(ISSUER, jsonFetch(lying))).rejects.toThrow(/claims issuer/);
  });

  it("refuses a document missing the endpoints PKCE needs", async () => {
    await expect(
      discover(ISSUER, jsonFetch({ issuer: ISSUER })),
    ).rejects.toThrow(/omits the endpoints/);
  });

  it("reports an issuer it cannot reach", async () => {
    const failing: FetchLike = async () => {
      throw new Error("network down");
    };
    await expect(discover(ISSUER, failing)).rejects.toThrow(/cannot discover/);
  });
});

describe("beginning authorization", () => {
  it("builds an S256 challenge and a fresh state", async () => {
    const request = await beginAuthorization({
      metadata: METADATA,
      client: BROWSER_CLIENT,
      scope: "openid",
    });

    const url = new URL(request.url);
    expect(url.origin + url.pathname).toBe(`${ISSUER}/authorize`);
    expect(url.searchParams.get("response_type")).toBe("code");
    expect(url.searchParams.get("client_id")).toBe("dashboard");
    expect(url.searchParams.get("redirect_uri")).toBe(BROWSER_CLIENT.redirect_uri);
    expect(url.searchParams.get("code_challenge_method")).toBe("S256");

    const challenge = url.searchParams.get("code_challenge") ?? "";
    expect(challenge).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(challenge).not.toBe(request.codeVerifier);

    // The challenge must actually be SHA-256 of the verifier, not a copy of it.
    const digest = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(request.codeVerifier),
    );
    const expected = btoa(String.fromCharCode(...new Uint8Array(digest)))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    expect(challenge).toBe(expected);
  });

  it("gives every request its own verifier and state", async () => {
    const options = { metadata: METADATA, client: BROWSER_CLIENT, scope: "openid" };
    const a = await beginAuthorization(options);
    const b = await beginAuthorization(options);
    expect(a.codeVerifier).not.toBe(b.codeVerifier);
    expect(a.state).not.toBe(b.state);
  });

  it("refuses a provider that does not offer S256", async () => {
    await expect(
      beginAuthorization({
        metadata: { ...METADATA, code_challenge_methods_supported: ["plain"] },
        client: BROWSER_CLIENT,
        scope: "openid",
      }),
    ).rejects.toThrow(/does not support PKCE S256/);
  });

  it("refuses a client not registered for the flow", async () => {
    await expect(
      beginAuthorization({
        metadata: METADATA,
        client: { ...BROWSER_CLIENT, flows: ["device_authorization"] },
        scope: "openid",
      }),
    ).rejects.toThrow(/not registered for authorization_code_pkce/);
  });
});

describe("completing authorization", () => {
  const tokenResponse = {
    access_token: "an-access-token",
    refresh_token: "a-refresh-token",
    token_type: "Bearer",
    expires_in: 3600,
    scope: "openid",
  };

  async function request() {
    return beginAuthorization({
      metadata: METADATA,
      client: BROWSER_CLIENT,
      scope: "openid",
    });
  }

  it("discards the refresh token by default", async () => {
    // A browser has nowhere to put one that an XSS cannot read.
    const prepared = await request();
    const token = await completeAuthorization({
      metadata: METADATA,
      client: BROWSER_CLIENT,
      request: prepared,
      code: "the-code",
      returnedState: prepared.state,
      fetch: jsonFetch(tokenResponse),
    });

    expect(token.accessToken).toBe("an-access-token");
    expect(token.expiresIn).toBe(3600);
    expect(JSON.stringify(token)).not.toContain("a-refresh-token");
    expect("refreshToken" in token).toBe(false);
  });

  it("returns the refresh token only when asked for explicitly", async () => {
    const prepared = await request();
    const token = await completeAuthorizationWithRefresh({
      metadata: METADATA,
      client: BROWSER_CLIENT,
      request: prepared,
      code: "the-code",
      returnedState: prepared.state,
      fetch: jsonFetch(tokenResponse),
    });
    expect(token.refreshToken).toBe("a-refresh-token");
  });

  it("sends the verifier and no client secret", async () => {
    const prepared = await request();
    let body = "";
    const capturing: FetchLike = async (_input, init) => {
      body = String(init?.body ?? "");
      return new Response(JSON.stringify(tokenResponse), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    };

    await completeAuthorization({
      metadata: METADATA,
      client: BROWSER_CLIENT,
      request: prepared,
      code: "the-code",
      returnedState: prepared.state,
      fetch: capturing,
    });

    const sent = new URLSearchParams(body);
    expect(sent.get("grant_type")).toBe("authorization_code");
    expect(sent.get("code_verifier")).toBe(prepared.codeVerifier);
    expect(sent.get("client_id")).toBe("dashboard");
    // A public client has no secret; sending one would mean it was shipped.
    expect(sent.get("client_secret")).toBeNull();
  });

  it("refuses a response whose state does not match the request", async () => {
    const prepared = await request();
    await expect(
      completeAuthorization({
        metadata: METADATA,
        client: BROWSER_CLIENT,
        request: prepared,
        code: "the-code",
        returnedState: "someone-elses-state",
        fetch: jsonFetch(tokenResponse),
      }),
    ).rejects.toThrow(/state does not match/);
  });

  it("reports the provider's error description when exchange fails", async () => {
    const prepared = await request();
    await expect(
      completeAuthorization({
        metadata: METADATA,
        client: BROWSER_CLIENT,
        request: prepared,
        code: "expired",
        returnedState: prepared.state,
        fetch: jsonFetch(
          { error: "invalid_grant", error_description: "code already used" },
          400,
        ),
      }),
    ).rejects.toThrow(/code already used/);
  });

  it("refuses a 200 response carrying no access token", async () => {
    const prepared = await request();
    await expect(
      completeAuthorization({
        metadata: METADATA,
        client: BROWSER_CLIENT,
        request: prepared,
        code: "the-code",
        returnedState: prepared.state,
        fetch: jsonFetch({ token_type: "Bearer" }),
      }),
    ).rejects.toThrow(/no access_token/);
  });
});
