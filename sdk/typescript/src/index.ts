/**
 * `@auru/pm` — a client for `auru-pm-v1` project-management providers.
 *
 * The endpoint is always yours to supply. Anyone can run a provider, so there
 * is no default host here and no registry lookup: which one to trust is the
 * user's decision.
 *
 * ```ts
 * import { AuruClient, createKernel } from "@auru/pm";
 *
 * const kernel = createKernel(binding);          // wasm in a browser, N-API in Node
 * const client = await AuruClient.connect({
 *   endpoint: "https://pm.example.com",
 *   accessToken,                                  // held in memory, never stored
 *   kernel,
 * });
 *
 * const project = client.project("user/night-drive");
 * for (const version of await project.history({ limit: 20 })) {
 *   console.log(version.timestamp, version.message);
 * }
 * ```
 *
 * A dashboard should read `Commit.metadata` — the ProjectInfo blob, a few
 * kilobytes — rather than the snapshot it points at, which for a real Live Set
 * is around 7 MB.
 */

export { PROTOCOL } from "./protocol.js";
export type {
  AuthMethod,
  AuthenticatedIdentity,
  AuthorIdentity,
  Capabilities,
  Commit,
  CommitSummary,
  ContentHash,
  HealthResponse,
  HistoryRange,
  OAuthClient,
  OAuthClientConfiguration,
  OAuthFlow,
  ProjectFormat,
  ProjectLocation,
  ProjectMetadata,
  ProjectProfile,
  ProviderProject,
  RetentionReport,
  RetentionRequest,
  RetentionRule,
  TreeRef,
} from "./protocol.js";

export { AuruError, HeadConflictError, isAuruError, isHeadConflict } from "./errors.js";
export type { ErrorCode } from "./errors.js";

export { AuruClient, ProjectClient } from "./transport.js";
export type { ClientOptions, FetchLike } from "./transport.js";

export {
  beginAuthorization,
  completeAuthorization,
  completeAuthorizationWithRefresh,
  discover,
  selectClient,
} from "./auth.js";
export type {
  AccessToken,
  AuthorizationRequest,
  AuthorizationServerMetadata,
  RefreshableToken,
} from "./auth.js";

export { createKernel } from "./kernel/api.js";
export { loadKernel } from "./kernel/load.js";
export type { LoadOptions } from "./kernel/load.js";
export { loadWasmKernel } from "./kernel/wasm.js";
export type { WasmSource } from "./kernel/wasm.js";
export type { KernelApi, KernelBinding } from "./kernel/api.js";
export type {
  ChangeKind,
  ChangeRow,
  ChangeTag,
  ChannelDiff,
  ChannelKind,
  ConflictedField,
  MergeOutcome,
  ProjectDiff,
  ProjectInfo,
} from "./kernel/types.js";
