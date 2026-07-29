//! Find every Ableton project inside a folder.
//!
//! What "Watch another folder" does: point it at a music drive and it reports
//! the projects it holds. Nothing is read into memory beyond each project's
//! location, and nothing is uploaded.
//!
//! ```text
//! cargo run -p auru-pm --example ableton_scan -- "/path/to/Ableton Projects"
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use auru_pm::ableton::{ScanOptions, scan_for_projects};

fn main() -> ExitCode {
    let Some(root) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ableton_scan <folder>");
        return ExitCode::FAILURE;
    };

    let started = Instant::now();
    let found = scan_for_projects(&root, &ScanOptions::default());
    let elapsed = started.elapsed();

    for bundle in found.iter().take(5) {
        println!(
            "  {}",
            bundle
                .root()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
    }
    if found.len() > 5 {
        println!("  … and {} more", found.len() - 5);
    }
    println!(
        "\n  {} project(s) found in {:.2}s",
        found.len(),
        elapsed.as_secs_f64()
    );
    ExitCode::SUCCESS
}
