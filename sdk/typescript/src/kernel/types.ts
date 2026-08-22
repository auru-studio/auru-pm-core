/**
 * Shapes the compute kernel returns.
 *
 * These mirror the Rust types in `auru-pm-kernel`, which serialize with
 * snake_case fields and snake_case enum values. Metadata bodies are left as
 * open records: they are per-DAW and grow as each adapter learns to read more,
 * and pinning every field here would mean this file has to change before a
 * dashboard can display a new one.
 */

export type ChangeKind = "add" | "remove" | "modify";

export type ChannelKind = "audio" | "midi" | "plugin" | "other";

export type ChangeTag =
  | "added"
  | "removed"
  | "renamed"
  | "moved"
  | "length"
  | "pitch"
  | "warp"
  | "clip_bpm"
  | "loop"
  | "content"
  | "plugin_preset"
  | "volume"
  | "pan"
  | "muted"
  | "solo"
  | "fx_added"
  | "fx_removed"
  | "fx_reordered"
  | "instrument_changed";

/** One change row inside a channel card. */
export interface ChangeRow {
  tag: ChangeTag;
  kind: ChangeKind;
  /** Clip or sub-target the change applies to. */
  target: string;
  /** Previous value, formatted for display. Absent on additions. */
  before: string | null;
  /** New value, formatted for display. Absent where meaningless, e.g. removals. */
  after: string | null;
}

export interface ChannelDiff {
  name: string;
  kind: ChannelKind;
  status: ChangeKind;
  clips_added: number;
  clips_removed: number;
  clips_modified: number;
  rows: ChangeRow[];
}

export interface ProjectDiff {
  /** Project-wide changes belonging to no single channel: tempo, key, markers. */
  project_changes: string[];
  /** Time signature of the *later* snapshot, as `[numerator, denominator]`. */
  time_sig: [number, number];
  channels: ChannelDiff[];
}

/**
 * The few-kilobyte summary of what a version is.
 *
 * Exactly one of the per-DAW fields is present. A dashboard should read this
 * rather than the snapshot: a real Live Set snapshot is around 7 MB and about a
 * hundred thousand elements, and this is the thing a commit points at so a
 * project list never has to fetch one.
 */
export interface ProjectInfo {
  schema: number;
  format: string;
  ableton?: Record<string, unknown>;
  flstudio?: Record<string, unknown>;
  dawproject?: Record<string, unknown>;
}

/** One field two sides changed differently. */
export interface ConflictedField {
  /** Dot-separated path; arrays use `[id=X]` or `.N`. */
  path: string;
  ancestor: unknown;
  local: unknown;
  remote: unknown;
}

/**
 * Result of a three-way merge, as a discriminated union.
 *
 * On `conflict`, `base` already has every disjoint change applied, so a caller
 * only has to resolve the listed fields.
 */
export type MergeOutcome =
  | { outcome: "clean"; merged: unknown }
  | { outcome: "conflict"; base: unknown; conflicts: ConflictedField[] };
