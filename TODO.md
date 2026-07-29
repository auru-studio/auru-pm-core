# TODO

What is not finished, grouped by what it costs the user. Each item says what
exists today and what is missing, so the gap is actionable rather than a
reminder that something is imperfect.

Verified against the tree on 2026-07-29. Items marked **verified** were
confirmed by running code, not by reading it. Where a version or file format is
described, it was read out of a real project rather than from documentation.

---

## 1. Blocking — nothing can actually be backed up

The whole product promise is unmet: the core can commit, merge, and restore,
and is well tested doing so, but no path from the UI reaches it.

### 1.1 The backup button is a timer, not a backup — **verified**

`back_up_all` ([main.rs:785](apps/auru-pm-ui/src/main.rs:785)) marks projects
as syncing, waits `TRANSFER_DURATION`, and marks them done. No provider is
contacted and no bytes move. The same is true of the per-project
`↑ BACK UP CHANGES` action.

Everything it needs already exists and is tested:
`push_with_freshness_check` ([sync.rs:400](crates/auru-pm/src/sync.rs:400)),
`FilesystemProvider`, `HttpProvider`, the integrity gate, and the stash-based
conflict path.

**Needed:** wire the UI action to `push_with_freshness_check`, run it off the
UI thread, and drive the progress bar from real transfer state instead of a
timer.

### 1.2 FL Studio commits would store zero samples — **verified**

`sample_manifest::plan_assets`
([sample_manifest.rs:155](crates/auru-pm/src/sample_manifest.rs:155)) branches
on Ableton and falls through to the native clip-path walk. An FL snapshot has
no native clips, so it plans **0 assets** — confirmed by probe. Committing an FL
project today would store the `.flp` and none of its audio, which is precisely
the bug the Ableton work fixed, reproduced for FL.

`flstudio::plan_bundle_assets` exists and is tested; it is simply not called
from the commit path.

**Needed:** dispatch to it in `plan_assets`, and add a manifest test per format
so a third DAW cannot repeat this.

### 1.3 Restore is not reachable from the UI

`ableton::restore_bundle` and `flstudio::restore::repoint` / `write_asset` are
implemented and tested. Nothing in `apps/auru-pm-ui` calls either, so a project
can never be brought back onto a second machine — the other half of the promise.

### 1.4 No provider is ever constructed

The UI never builds a `FilesystemProvider` or `HttpProvider`, never reads a
sidecar's `primary`, and never writes one. Provider selection in Settings is
display-only.

---

## 2. Authentication and providers

### 2.1 The provider catalogue is stubbed

`stub_provider_catalog` ([catalog.rs:249](apps/auru-pm-ui/src/catalog.rs:249))
supplies the list unless `--providers-file` is passed. There is no live fetch
from `AURU_REGISTRY_URL` in the running app.

### 2.2 OAuth and token storage are unwired

`oauth::start_device_flow` and the `token_store` module are implemented; no UI
code references either. The Add-Provider flow collects a token and discards it.

### 2.3 The reference server has no authentication

`auru-pm-server` advertises `auth_methods: ["none"]`
([main.rs:66](crates/auru-pm-server/src/main.rs:66)) and keeps state in memory.
It is a conformance target, not a deployable service. Missing: persistence,
any auth, and rate limiting.

### 2.4 `auru-pm-client` is an empty shell

11 lines, no public API. Either build it or delete it — an empty crate in the
workspace implies a component that does not exist.

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

Effectively unsupported beyond storage. It normalises, commits, merges, and
restores, but:

- No metadata reader — `ProjectInfo::from_snapshot` returns `None`, so the
  library shows no tempo, key, or track count.
- No asset extraction — samples inside a `.dawproject` are not planned or
  vendored, the same class of gap as 1.2.
- No plugin inventory.
- No structured diff — falls through to the format-agnostic summary.
- Not offered by discovery: `scan_for_projects` finds Ableton folders and
  `.flp` files only, so a `.dawproject` can be added by hand but never found.

### 3.4 Native `.auru` format

- No `ProjectInfo` summary, so native projects show no detail.
- `plan_assets` keeps raw clip paths as manifest keys, deliberately, to avoid
  changing existing commit ids. Means native projects get no vendoring and no
  path rewriting on restore.

---

## 4. UI gaps

- **Version history is always empty.** `Project.versions` is `&'static []` at
  every construction site; `[ RECENT VERSIONS ]` and `VIEW FULL HISTORY →`
  render against nothing. `list_history` exists on every provider.
- **`syncing · 64%` is hardcoded** ([model.rs:925](apps/auru-pm-ui/src/model.rs:925)).
- **Version retention does nothing.** The setting persists and is documented as
  display-only ([main.rs:146](apps/auru-pm-ui/src/main.rs:146)); no pruning runs.
  `Cas` GC (`collect_reachable`, `GcReport`) exists and is unused by the app.
- **Onboarding is one step, not the designed three.** The provider-connection
  and folder-selection steps from `auru-pm-claude-design` are not built.
- **Recovery mode is a route with no implementation behind it.**
- **`SyncDirection::UpstreamAhead`, `ProjectStatus::{NotDownloaded, Conflicted}`
  are `#[allow(dead_code)]`** — unreachable until a provider is connected. Their
  screens are written; nothing can produce the states.
- **Project status is inferred from modification time.** `ProjectStatus::read_from_disk`
  compares the project's mtime against the sidecar's, so it means "you have saved
  since your last backup" rather than "the contents differ". Documented, and
  errs toward offering a no-op backup — the harmless direction.
- **Sorting by Last Modified (Remote) falls back to alphabetical**, because no
  project has a backup time until 1.1 lands.
- **The FL import flow and detail page have never been used.** They compile,
  are unit-tested, and the app launches, but no one has clicked through them.

---

## 5. Cross-cutting

- **Compressed uploads are never negotiated in practice.** `Capabilities::compressed_uploads`
  is implemented on both sides and defaults to false; no deployed provider sets it.
- **`AURU_ABLETON_PATH_ALIASES` is still Ableton-named** although FL uses the
  same mechanism. Plan called for a DAW-neutral `AURU_PATH_ALIASES` with the old
  name kept as an alias; not yet done.
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
```
