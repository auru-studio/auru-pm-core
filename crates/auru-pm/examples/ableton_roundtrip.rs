//! Commit a real Ableton project folder and restore it somewhere else.
//!
//! Proves the round trip end to end: gather the project and everything it
//! references, then rebuild it in a fresh directory and check that the
//! restored Live Set resolves its own media with no path aliases — i.e. that
//! it would open on a machine that had never seen the original.
//!
//! The source project is only read.
//!
//! ```text
//! AURU_ABLETON_PATH_ALIASES='E:/Music Production=/mnt/ssd/Music Production' \
//!   cargo run -p auru-pm --example ableton_roundtrip -- "/path/to/Song Project"
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use auru_pm::ableton::{self, BundlePolicy};
use auru_pm::{
    AuthorIdentity, ContentHash, FilesystemProvider, ProjectProvider, ProjectSnapshot, PushOutcome,
    SampleEntry, SampleManifest, push_with_freshness_check, sidecar_path_for,
};

#[tokio::main]
async fn main() -> ExitCode {
    let Some(source) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ableton_roundtrip <project folder>");
        return ExitCode::FAILURE;
    };

    let Some(bundle) = ableton::AbletonBundle::detect(&source).ok().flatten() else {
        eprintln!("'{}' is not an Ableton project folder", source.display());
        return ExitCode::FAILURE;
    };
    let live_set_name = bundle
        .live_set()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Song.als")
        .to_owned();

    let snapshot = match ProjectSnapshot::load(bundle.live_set()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("could not read the Live Set: {error}");
            return ExitCode::FAILURE;
        }
    };

    let policy = BundlePolicy::default();
    let plan = match ableton::plan_bundle_assets(&snapshot, &source, &policy) {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            eprintln!("no project folder detected");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not plan assets: {error}");
            return ExitCode::FAILURE;
        }
    };

    let temp = tempfile::tempdir().expect("temporary workspace");
    let provider = FilesystemProvider::open(temp.path().join("provider")).expect("provider");

    // Store the planned assets and build the manifest the commit points at.
    let mut manifest = SampleManifest::new();
    let mut stored_bytes = 0_u64;
    for asset in &plan.assets {
        let bytes = match std::fs::read(&asset.source) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("  skipping {}: {error}", asset.bundle_path);
                continue;
            }
        };
        let hash = ContentHash::of(&bytes);
        provider.put_blob(&hash, &bytes).await.expect("put asset");
        stored_bytes += bytes.len() as u64;
        manifest.insert(SampleEntry {
            path: asset.bundle_path.clone(),
            hash,
            size: bytes.len() as u64,
            kind: asset.kind,
            origin: asset.origin.clone(),
        });
    }

    let sidecar = sidecar_path_for(&temp.path().join(&live_set_name));
    let PushOutcome::Committed { commit_id, .. } = push_with_freshness_check(
        &provider,
        &provider.provider_id(),
        &[],
        &sidecar,
        snapshot.as_bytes(),
        AuthorIdentity {
            display_name: "roundtrip".into(),
            provider_user_id: "roundtrip".into(),
            provider_id: "local".into(),
            email: None,
        },
        "Round trip",
        "",
    )
    .await
    .expect("push") else {
        eprintln!("unexpected conflict on a first commit");
        return ExitCode::FAILURE;
    };

    let manifest_bytes = manifest.canonical_encoding().expect("encode manifest");
    let manifest_hash = ContentHash::of(&manifest_bytes);
    provider
        .put_blob(&manifest_hash, &manifest_bytes)
        .await
        .expect("put manifest");
    let mut commit = provider.get_commit(&commit_id).await.expect("get commit");
    commit.tree.samples = manifest_hash;

    // Restore into a fresh directory.
    let destination = temp.path().join("Restored Project");
    let report =
        match ableton::restore_bundle(&provider, &commit, &destination, &live_set_name).await {
            Ok(report) => report,
            Err(error) => {
                eprintln!("restore failed: {error}");
                return ExitCode::FAILURE;
            }
        };

    println!("== committed ==");
    println!(
        "  {} files · {:.1} MiB",
        plan.assets.len(),
        mib(stored_bytes)
    );
    println!(
        "  {} gathered from outside the folder",
        plan.vendored().count()
    );

    println!("\n== restored to a fresh folder ==");
    println!(
        "  {} files written · {:.1} MiB",
        report.files_written,
        mib(report.bytes_written)
    );
    println!(
        "  references: {} repointed · {} already in folder · {} left to Live · {} empty",
        report.rewrite.rewritten,
        report.rewrite.already_in_folder,
        report.rewrite.left_to_live,
        report.rewrite.empty
    );

    // The real question: does the restored set find its own media with no
    // aliases configured — as it would on a machine that never saw the source?
    let restored = ProjectSnapshot::load(&report.live_set).expect("load restored set");
    let restored_bundle = ableton::AbletonBundle::detect(&destination)
        .expect("detect")
        .expect("restored folder is a project");
    let bare = BundlePolicy {
        path_aliases: Vec::new(),
        ..BundlePolicy::default()
    };

    let mut resolved = 0;
    let mut unresolved = Vec::new();
    for asset in ableton::read_asset_refs(&restored).expect("refs") {
        if asset.is_unresolvable() || asset.class == ableton::RefClass::Library {
            continue;
        }
        if restored_bundle
            .resolve(&asset.relative_path, &asset.absolute_path, &bare)
            .is_some()
        {
            resolved += 1;
        } else {
            unresolved.push(asset.dedup_key().to_owned());
        }
    }

    println!("\n== would it open elsewhere? ==");
    println!("  {resolved} reference(s) resolve inside the restored folder");
    unresolved.sort();
    unresolved.dedup();
    if unresolved.is_empty() {
        println!("  nothing missing");
    } else {
        println!("  {} still missing:", unresolved.len());
        for reference in &unresolved {
            println!("    {reference}");
        }
    }

    let metadata = ableton::read_metadata(&restored).expect("metadata");
    println!(
        "\n  restored set reads as {} BPM · {} · {} tracks · {}",
        metadata.tempo.unwrap_or_default(),
        metadata
            .time_signature
            .map_or_else(|| "?".to_owned(), |sig| sig.to_string()),
        metadata.tracks.total(),
        metadata.live_version.as_deref().unwrap_or("unknown")
    );

    ExitCode::SUCCESS
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
