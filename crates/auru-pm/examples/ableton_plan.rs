//! Show what a commit of an Ableton project folder would capture.
//!
//! Reports the folder's own contents, the files that would be gathered in from
//! outside it, and anything referenced that could not be found on this
//! machine. Reads only — nothing is written or uploaded.
//!
//! ```text
//! cargo run -p auru-pm --example ableton_plan -- "/path/to/Song Project"
//! ```
//!
//! Projects saved on another machine reference paths that do not exist here.
//! Bridge them with `AURU_ABLETON_PATH_ALIASES`, which maps a recorded prefix
//! onto a local one:
//!
//! ```text
//! AURU_ABLETON_PATH_ALIASES='E:/Music Production=/mnt/ssd/Music Production'
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use auru_pm::ableton::{self, BundlePolicy};
use auru_pm::{ProjectFormat, ProjectSnapshot};

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ableton_plan <project folder | project.als>");
        return ExitCode::FAILURE;
    };

    let Some(bundle) = ableton::AbletonBundle::detect(&path).ok().flatten() else {
        eprintln!(
            "'{}' is not an Ableton project folder.\n\
             Pass the folder containing the .als, or a .als that lives in one.",
            path.display()
        );
        return ExitCode::FAILURE;
    };

    let snapshot = match ProjectSnapshot::load(bundle.live_set()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("could not read '{}': {error}", bundle.live_set().display());
            return ExitCode::FAILURE;
        }
    };
    if snapshot.format() != ProjectFormat::AbletonLiveSet {
        eprintln!("'{}' is not a Live Set", bundle.live_set().display());
        return ExitCode::FAILURE;
    }

    let policy = BundlePolicy::default();
    if policy.path_aliases.is_empty() {
        eprintln!(
            "note: no path aliases set. References to another machine's drives \
             will show as not found.\n      \
             See AURU_ABLETON_PATH_ALIASES in this example's docs.\n"
        );
    }

    let plan = match ableton::plan_bundle_assets(&snapshot, &path, &policy) {
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

    println!("== {} ==", bundle.root().display());
    println!("live set   {}", bundle.live_set().display());
    println!(
        "snapshot   {} KiB canonical JSON (stored separately)\n",
        snapshot.as_bytes().len() / 1024
    );

    println!("-- would be committed --");
    for asset in &plan.assets {
        let size = std::fs::metadata(&asset.source)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let marker = if asset.is_vendored() {
            "gathered"
        } else {
            "in-folder"
        };
        println!(
            "  [{marker:^9}] {:>9} KiB  {}",
            size / 1024,
            asset.bundle_path
        );
        if let Some(origin) = &asset.origin {
            println!("                             ← {origin}");
        }
    }

    if !plan.unresolved.is_empty() {
        println!("\n-- referenced but not found on this machine --");
        for missing in &plan.unresolved {
            println!("  {}", missing.reference);
        }
    }

    let gathered = plan.vendored().count();
    println!(
        "\n  {} file(s) · {:.1} MiB total",
        plan.assets.len(),
        plan.total_bytes() as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  {gathered} gathered from outside the folder · {} not found",
        plan.unresolved.len()
    );

    ExitCode::SUCCESS
}
