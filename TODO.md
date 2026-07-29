# TODO

What is not finished, grouped by what it costs the user. Each item says what
exists today and what is missing, so the gap is actionable rather than a
reminder that something is imperfect.

Verified against the tree on 2026-07-29. Items marked **verified** were
confirmed by running code, not by reading it. Where a version or file format is
described, it was read out of a real project rather than from documentation.

## Implemented 2026-07-29

- The desktop app now constructs per-project `FilesystemProvider` and
  `HttpProvider` instances, calls `push_with_freshness_check` off the UI thread,
  records the successful primary in the sidecar, and reports real completion
  or conflict outcomes. The timer-based backup simulation is gone.
- FL Studio commits now dispatch through `flstudio::plan_bundle_assets`; a
  regression test proves a canonical FL snapshot produces a sample manifest.
- Provider connections persist. PATs and OAuth tokens are stored in the OS
  keychain, and OAuth uses the real device-code progress stream.
- Provider-scoped project handles persist in the sidecar, so moving a project
  and its sidecar does not create a second remote history.
- Provider accounts now expose typed project catalogues over filesystem and
  HTTP providers. Provider-only projects enter the library as downloadable,
  and a complete recovery writes an enrolled local copy with its provider
  identity and HEAD intact.
- Recent versions come from `list_history`, and Restore now fetches the selected
  commit into a new folder without overwriting the working project. Native,
  DAWproject, Ableton, and FL restore paths are wired; FL restore materialises
  and repoints captured samples.
- The operating system's registered DAW now handles Open Project.
- Provider catalogue fetching uses `AURU_REGISTRY_URL`; the old stub catalogue
  is gone.
- Discovery now includes `.dawproject` and native `.auru` files.
- `AURU_PATH_ALIASES` is the DAW-neutral environment variable;
  `AURU_ABLETON_PATH_ALIASES` remains a compatibility fallback.
- Version retention now runs after successful backups. Filesystem and HTTP
  providers expose a capability-gated retention operation; local repositories
  truncate visible history and garbage-collect unreachable commit and project
  objects, while hosted providers can enforce and collect with their own grace
  policy. The desktop persists the preference and reports pruning failures
  without misreporting the already-successful backup as failed.
- The upload-verification preference now performs an explicit provider-side
  re-read after each committed backup. It validates the commit id, snapshot,
  asset manifest, project metadata, every referenced asset hash, and recorded
  asset sizes; a failed verification is reported as a warning without
  misreporting the already-successful upload as failed.
- Production `DEMO` labels, routes, fake actions, and test-helper names were
  removed. The premature recovery route and custom-provider CTA were removed
  rather than claiming to work. The DAWproject oracle fixture still contains a
  clip named `Demo`; that is inert test data inside the archive, not product UI.
- DAWproject now has a schema-backed metadata reader, embedded-media manifest
  entries, VST2/VST3/CLAP/AU plugin inventory, missing-plugin UI dispatch, and
  per-track structured version diffs. The existing oracle is exercised through
  commit metadata and the desktop detail model.

---

## 1. Blocking product loop — **resolved 2026-07-29**

Backup, provider discovery, history, download, and restore are now reachable
from the desktop app for local providers and HTTP providers advertising the
`project_listing` capability.

### 1.1 Backup button — **resolved 2026-07-29**

Both single-project and Back Up All actions now run the real coordinator in the
background. The row is marked in-flight until that operation returns; no timer
advances it.

### 1.2 FL Studio sample commits — **resolved 2026-07-29**

`sample_manifest::plan_assets` reconstructs the FL stream and dispatches to
`flstudio::plan_bundle_assets`. The UI backup/restore integration test proves
the sample blob is captured, materialised, and repointed.

### 1.3 Restore — **resolved 2026-07-29**

Recent-history rows restore their commit into a new folder. A connected
provider account lists provider-only projects; downloading one restores it
into a new folder and writes the sidecar required for subsequent sync.

### 1.4 Provider construction — **resolved 2026-07-29**

The UI constructs both provider types, reads a project's sidecar primary,
persists the primary after a successful commit, and persists each provider's
opaque project handle. Adding a local folder connects it and makes it the
default destination immediately.

---

## 2. Authentication and providers

### 2.1 Provider catalogue — **resolved 2026-07-29**

The app fetches `AURU_REGISTRY_URL` in the background with a 24-hour cache;
`--providers-file` remains the explicit override.

### 2.2 OAuth and token storage — **resolved 2026-07-29**

PAT and device-code OAuth tokens are stored as provider-account credentials in
the OS keychain. Project-scoped credentials remain supported by the core.
OAuth completion is confirmed by the provider. The protocol has no
provider-wide PAT validation endpoint, so PAT copy now says honestly that the
token will be checked by the first project request.

### 2.3 The reference server has no authentication

`auru-pm-server` advertises `auth_methods: ["none"]`
([main.rs:74](crates/auru-pm-server/src/main.rs:74)) and keeps state in memory.
It is a conformance target, not a deployable service. Missing: persistence,
any auth, and rate limiting.

### 2.4 `auru-pm-client` — **resolved 2026-07-29**

`ProviderAccount` is the account-level client API: it lists projects before a
handle is known and opens the selected project as a project-scoped provider.
The desktop app consumes this API for filesystem and HTTP recovery.

### 2.5 Teams surface is declared but unimplemented

`list_members` and `permissions` return `Error::Unsupported` on every provider,
and `Capabilities::members` / `permissions` are always false. The trait hooks
are deliberate placeholders for the teams plan; nothing consumes them yet.

---

## 3. Per-DAW gaps

### 3.1 FL Studio

- **Mixer inserts merge coarsely.** FL delimits inserts with no cursor event —
  in a real project the entire mixer arrives as one 56 KB blob under event 225 —
  so `tree.rs` leaves them ungrouped rather than inventing a boundary the merge
  would then act on. Correct but coarse: any mixer edit conflicts wholesale.
  Needs event 225's internal layout worked out against more real projects.
- **Time signature is inferred from two events seen only as 4/4.** Events 17 and
  18 read as numerator and denominator, but both sampled projects are in 4/4, so
  the mapping is unconfirmed. A project in 3/4 or 6/8 would settle it.
- **No note-level or playlist diff.** Pattern contents and playlist arrangement
  are stored as opaque payloads and not compared. Deliberate for now — a diff
  reporting thousands of changed floats would bury the real change — but it means
  "nothing changed" can be reported for a project whose notes were rewritten.
- **Plugin binary paths are found by scanning, not parsing.** `binary_path`
  ([plugins.rs](crates/auru-pm/src/flstudio/plugins.rs)) hunts for a printable
  run ending in a known extension inside event 213. It works on both real
  projects but is a heuristic; a plugin storing paths differently is identified
  only by its display name.
- **`ProjDataPath` (event 202) is ignored.** Both sampled projects leave it
  empty. A project that uses FL's project-data folder is untested.

### 3.2 Ableton

- **`ChangeTag::Solo` is never emitted.** A Live 12 set carries no track-level
  solo state, only `Mixer/SoloSink` and a set-wide `SoloOrPflSavedValue`, and
  guessing which means "soloed" would produce wrong history
  ([diff.rs:499](crates/auru-pm/src/ableton/diff.rs:499)). Needs a real set
  saved with a soloed track to resolve.
- **Diff omits notes, warp markers, and automation.** Per-note MIDI, warp
  markers, and envelope changes are not compared; a project whose notes changed
  can show an empty diff.
- **Plugin parameter changes are not diffed.** One sampled project held 3,072
  `PluginFloatParameter` nodes. Intentional, same reasoning as FL.
- **Live 9 keeps stale duplicate references, and they read as missing.** A Live
  9 set records two `FileRef`s per collected sample: a working
  `RelativePathType=3` pointing at the copy in `Samples/Imported/`, and a
  `RelativePathType=1` recording where the file originally came from. The second
  usually resolves to nothing, so a restore of an intact project reports files
  as missing — 15 of them on the project checked. Cosmetic rather than a data
  problem, but it undermines trust in the one report that must be believed.
  Needs the stale partner suppressed when a working reference to the same file
  name exists.
- **Live 9/10 absolute paths on macOS are not recoverable.** Those versions wrote
  no `<Path>`; the location lives in `<Data>`, as UTF-16 hex on Windows (now
  decoded) and as a binary alias record on macOS (deliberately not decoded — a
  fabricated path is worse than none). A Mac-authored Live 9 project whose
  samples sit outside the folder can therefore only be resolved by its relative
  path. Parsing the alias record would close the gap.

**Resolved 2026-07-29** — real Live 9.1.7 project round-tripped end to end
(150 BPM · 4/4 · 20 tracks), confirming the `MasterTrack`/`MainTrack` accessor
on a genuine pre-Live-12 set. Two bugs it exposed, both fixed:
Live 9/10 references lost their file names (2,049 references collapsed to 25
"distinct files" instead of 52), and macOS `._` resource forks counted as Live
Sets, making a Mac-authored project unopenable. A survey of 400 projects in the
library found 235 Live 9, 137 Live 10, 28 Live 12 — so 93% used the affected
reference format.

### 3.3 DAWproject

**Implemented 2026-07-29; one storage follow-up remains.** The semantic reader
follows the official DAWproject 1.0 schema and now supplies:

- `ProjectInfo` metadata for title/credits, exporting application, tempo, time
  signature, tracks, clips, scenes, markers, arrangement extent, plugins, and
  media. The 1.0 schema has no project-wide key, so the UI leaves it blank.
- One manifest/CAS object per referenced embedded media file. Restore fetches
  those objects and hydrates the archive from them; external files and missing
  archive entries remain explicitly distinguishable. The v1 canonical
  snapshot also retains an inline fallback so the existing provider-free
  `ProjectSnapshot::restore_bytes` API stays valid.
- Stable plugin identities from the interchange format: VST2 decimal IDs,
  VST3 UUIDs, CLAP textual IDs, AU IDs, and DAW-scoped built-ins. The desktop
  now runs these through the normal missing-plugin resolver.
- Structured version diffs for musical metadata, tracks, clips, clip content,
  mix parameters, devices, plugin inventory, and embedded resources. Unknown
  XML still emits a generic change rather than disappearing.

**Production interoperability proof, 2026-07-29:** paired Bitwig Studio 5.3.13
native projects and DAWproject exports were checked. The MIDI-only export reads
as 4 song tracks, 1 master, 1 visible clip, 8 scenes, and no media. Its edited
version reads as 5 song tracks, 1 master, 3 visible clips, 8 scenes, and 2
embedded audio files. Both preserve the same CLAP and built-in device
identities, normalize/restore unchanged, commit successfully, and restore
unchanged after hydrating media from their individual CAS objects. The two
embedded WAV payloads are byte-identical to Bitwig's source samples, and the
CLAP preset payload is byte-identical between the native and interchange
archives.

That pair also proved Bitwig renumbers later XML ids when inserting a track and
renumbers clip-local content wrappers even when their notes are unchanged.
Track matching now prefers stable role/name/device evidence before ids, and
generated ids are ignored when comparing clip and device content. The source
archives are not committed because they contain sample-pack audio, third-party
preset state, and machine-local paths; the exporter behavior is captured by a
small synthetic regression instead.

- **Remaining:** making embedded media truly lazy, rather than storing the v1
  inline fallback as well as its CAS object, requires a versioned snapshot
  wrapper that can declare provider-hydrated archive resources.
- **Resolved 2026-07-29:** discovery now offers `.dawproject` files (and native
  `.auru` files) alongside Ableton and FL projects.

### 3.4 Native `.auru` format

- No `ProjectInfo` summary, so native projects show no detail.
- **Resolved 2026-07-29:** `plan_assets` keeps raw clip paths as manifest keys
  to preserve existing commit ids; restore now materialises those committed
  blobs under a safe `Samples/` path and rewrites every recovered clip.

---

## 4. UI gaps

- **Resolved 2026-07-29:** recent version history is populated from
  `list_history`, and each row carries the real commit id used by Restore.
- **Resolved 2026-07-29:** the hardcoded `syncing · 64%` value and timer-driven
  progress were removed.
- **Resolved 2026-07-29:** version retention is applied after every successful
  backup. “Last 50” and “last year” move a permanent provider-side history
  boundary; filesystem providers also garbage-collect unreachable commit,
  snapshot, metadata, manifest, and sample objects after a one-hour safety
  window. Merge ancestry, queued mirror commits, and stashed blobs are explicit
  GC roots, and HEAD publication is locked against collection. HTTP providers
  negotiate the `history_retention` capability and use
  `POST /v1/projects/{handle}/retention`. The UI persists all three backup
  settings and reports unsupported providers or pruning failures separately
  from backup success.
- **Automatic backups are still a preference without a scheduler.** The
  setting now survives restarts, but nothing watches project files, waits for
  the advertised five quiet minutes, or calls the backup coordinator. Until
  that watcher exists, the switch must not be mistaken for automatic
  protection.
- **Resolved 2026-07-29:** “Verify every copy after upload” now re-reads the
  committed object graph through the selected provider and validates the
  commit, snapshot, manifest, metadata, every asset hash, and asset size. A
  failed verification produces a warning while retaining the successful
  backup and its history.
- **Onboarding is one step, not the designed three.** The provider-connection
  and folder-selection steps from `auru-pm-claude-design` are not built.
- **Resolved 2026-07-29:** the `project_listing` capability adds
  `GET /v1/projects` and project profiles. New-machine projects appear in the
  normal library, and Download restores and enrolls the selected project.
- **`ProjectStatus::Conflicted` is now reachable** from a real coordinator
  outcome, but the field-by-field resolver is still only a notification.
  `SyncDirection::UpstreamAhead` is produced when refreshing a known project's
  history, and Download Latest restores that head to a new folder.
  `ProjectStatus::NotDownloaded` is produced by provider project discovery.
- **Project status is inferred from modification time.** `ProjectStatus::read_from_disk`
  compares the project's mtime against the sidecar's, so it means "you have saved
  since your last backup" rather than "the contents differ". Documented, and
  errs toward offering a no-op backup — the harmless direction.
- **Resolved 2026-07-29:** Last Modified (Remote) uses sidecar modification time
  for local projects and the provider HEAD commit timestamp for provider-only
  projects.
- **The FL import flow and detail page have never been used.** They compile,
  are unit-tested, and the app launches, but no one has clicked through them.

---

## 5. Cross-cutting

- **Compressed uploads are never negotiated in practice.** `Capabilities::compressed_uploads`
  is implemented on both sides and defaults to false; no deployed provider sets it.
- **Resolved 2026-07-29:** `AURU_PATH_ALIASES` is the primary name and
  `AURU_ABLETON_PATH_ALIASES` remains a fallback.
- **Path aliases have no UI.** Resolving a project saved on another machine
  requires setting an environment variable, which no musician will do. This is
  the difference between "your samples were found" and "10 files could not be
  located" for any cross-machine restore.
- **Scanning cost grew with FL support.** A single-pass walk of a real 655-project
  drive takes ~450 ms, against ~50 ms when only Ableton folders were searched;
  finding files means descending into sample-pack directories that a
  folder-only scan could skip.
- **No end-to-end test runs against real projects in CI.** The real `.als` and
  `.flp` files live outside the repo and one is 18 MB, so the round-trip proofs
  are `cargo run --example` commands a person has to run.

  This is the highest-value item on the list. Every serious bug found so far came
  from running a real file, and none would have been caught by a hand-written
  fixture, because each was a case nobody thought to write: Live 9 keeping file
  names in a sibling element, macOS `._` companions ending in `.als`, an FL
  plugin path followed by a printable length byte. A small committed corpus —
  one project per Live major version, one per FL version, deliberately including
  a Mac-authored one — would turn all of those into regressions rather than
  discoveries.

---

## Verification commands

```bash
cargo fmt --all -- --check && cargo test --workspace --locked && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

The UI is an excluded nested workspace and is checked separately:

```bash
cd apps/auru-pm-ui && cargo fmt -- --check && cargo test --offline && cargo clippy --all-targets --offline -- -D warnings
```

Real-project proofs, run by hand:

```bash
cargo run --example flp_roundtrip -- "/path/to/Project.flp"
cargo run --example dawproject_inspect -- "/path/to/Project.dawproject"
```
