/**
 * Error mapping.
 *
 * Providers are written by third parties, so the client has to behave when one
 * sends something outside the table — or something that is not JSON at all.
 * A proxy returning an HTML 502 is the ordinary case, not a hypothetical.
 */

import { describe, expect, it } from "vitest";

import { AuruClient } from "../src/transport.js";
import { HeadConflictError, isAuruError, isHeadConflict } from "../src/errors.js";
import type { FetchLike } from "../src/transport.js";

const HEALTH = {
  protocol: "auru-pm-v1",
  provider_id: "stub",
  capabilities: {
    project_listing: true,
    members: false,
    permissions: false,
    branches: false,
    server_side_merge: false,
    compressed_uploads: false,
    history_retention: false,
    project_scoped_blobs: true,
    auth_methods: ["none"],
  },
};

/** A provider that answers health normally and everything else as given. */
function stub(answer: () => Response): { fetch: FetchLike; seen: Request[] } {
  const seen: Request[] = [];
  const fetchImpl: FetchLike = async (input, init) => {
    seen.push(new Request(input, init));
    if (input.endsWith("/v1/health")) {
      return new Response(JSON.stringify(HEALTH), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    return answer();
  };
  return { fetch: fetchImpl, seen };
}

async function clientAnswering(answer: () => Response): Promise<AuruClient> {
  return AuruClient.connect({ endpoint: "https://pm.example.com", fetch: stub(answer).fetch });
}

function json(body: unknown, status: number, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

describe("mapping a provider's error body", () => {
  it("keeps the code and message a provider sent", async () => {
    const client = await clientAnswering(() =>
      json({ code: "forbidden", message: "not your project" }, 403),
    );
    try {
      await client.me();
      throw new Error("expected a failure");
    } catch (error) {
      expect(isAuruError(error)).toBe(true);
      if (!isAuruError(error)) return;
      expect(error.code).toBe("forbidden");
      expect(error.message).toBe("not your project");
      expect(error.status).toBe(403);
      expect(error.retryable).toBe(false);
    }
  });

  it("surfaces head_conflict as its own type, carrying the current HEAD", async () => {
    const current = `blake3:${"c".repeat(64)}`;
    const client = await clientAnswering(() =>
      json({ code: "head_conflict", current }, 409),
    );

    try {
      await client.project("p").advanceHead(null, `blake3:${"d".repeat(64)}`);
      throw new Error("expected a conflict");
    } catch (error) {
      expect(error).toBeInstanceOf(HeadConflictError);
      if (!isHeadConflict(error)) return;
      expect(error.current).toBe(current);
    }
  });

  it("reads a null current HEAD on an empty project", async () => {
    const client = await clientAnswering(() =>
      json({ code: "head_conflict", current: null }, 409),
    );
    try {
      await client.project("p").advanceHead(`blake3:${"e".repeat(64)}`, `blake3:${"f".repeat(64)}`);
      throw new Error("expected a conflict");
    } catch (error) {
      if (!isHeadConflict(error)) throw error;
      expect(error.current).toBeNull();
    }
  });

  it("carries Retry-After on a rate limit, and marks it retryable", async () => {
    const client = await clientAnswering(() =>
      json({ code: "rate_limited", message: "slow down" }, 429, { "retry-after": "60" }),
    );
    try {
      await client.me();
      throw new Error("expected a failure");
    } catch (error) {
      if (!isAuruError(error)) throw error;
      expect(error.code).toBe("rate_limited");
      expect(error.retryAfter).toBe(60);
      expect(error.retryable).toBe(true);
    }
  });

  it("treats an unreachable identity provider as retryable, not as a bad token", async () => {
    // Re-prompting the user here would be wrong: the token was never judged.
    const client = await clientAnswering(() =>
      json({ code: "authentication_unavailable", message: "idp down" }, 503),
    );
    try {
      await client.me();
      throw new Error("expected a failure");
    } catch (error) {
      if (!isAuruError(error)) throw error;
      expect(error.code).toBe("authentication_unavailable");
      expect(error.retryable).toBe(true);
    }
  });

  it("falls back to the status when a provider invents a code", async () => {
    const client = await clientAnswering(() =>
      json({ code: "teapot", message: "unusual" }, 404),
    );
    try {
      await client.me();
      throw new Error("expected a failure");
    } catch (error) {
      if (!isAuruError(error)) throw error;
      expect(error.code).toBe("not_found");
      expect(error.message).toBe("unusual");
    }
  });

  it("survives a proxy answering with HTML instead of JSON", async () => {
    const client = await clientAnswering(
      () => new Response("<html>502 Bad Gateway</html>", { status: 502 }),
    );
    try {
      await client.me();
      throw new Error("expected a failure");
    } catch (error) {
      if (!isAuruError(error)) throw error;
      expect(error.code).toBe("internal");
      expect(error.retryable).toBe(true);
      expect(error.message).toContain("502");
    }
  });
});

describe("capability gating", () => {
  it("refuses an unadvertised capability before spending a round trip", async () => {
    const { fetch: fetchImpl, seen } = stub(() => json({}, 200));
    const client = await AuruClient.connect({
      endpoint: "https://pm.example.com",
      fetch: fetchImpl,
    });

    // `history_retention` is false in the stub's capabilities.
    await expect(
      client.project("p").pruneHistory({ rule: { policy: "latest", count: 10 } }),
    ).rejects.toThrow(/does not support history retention/);

    // Only the health request was ever sent.
    expect(seen.map((request) => new URL(request.url).pathname)).toEqual(["/v1/health"]);
  });
});

describe("protocol version", () => {
  it("refuses a provider speaking a different wire version", async () => {
    const fetchImpl: FetchLike = async () =>
      json({ ...HEALTH, protocol: "auru-pm-v2" }, 200);

    await expect(
      AuruClient.connect({ endpoint: "https://pm.example.com", fetch: fetchImpl }),
    ).rejects.toThrow(/speaks auru-pm-v2/);
  });
});

describe("the bearer token", () => {
  it("is sent on every request and never exposed", async () => {
    const { fetch: fetchImpl, seen } = stub(() => json({ commit_id: null }, 200));
    const client = await AuruClient.connect({
      endpoint: "https://pm.example.com",
      accessToken: "secret-token",
      fetch: fetchImpl,
    });

    await client.project("p").head();
    const request = seen.at(-1);
    expect(request?.headers.get("authorization")).toBe("Bearer secret-token");

    expect(client.authenticated).toBe(true);
    expect(JSON.stringify(client)).not.toContain("secret-token");

    client.setAccessToken(undefined);
    expect(client.authenticated).toBe(false);
    await client.project("p").head();
    expect(seen.at(-1)?.headers.get("authorization")).toBeNull();
  });
});
