//! List every project under a folder, whatever DAW made it.
//!
//! ```text
//! cargo run --example scan_projects -- "/mnt/music"
//! ```

use auru_pm::discovery::{self, ScanOptions};

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: scan_projects <folder>...");
        std::process::exit(2);
    }

    for root in &roots {
        let started = std::time::Instant::now();
        let found =
            discovery::scan_for_projects(std::path::Path::new(root), &ScanOptions::default());
        println!(
            "\n{root} — {} project(s) in {:?}",
            found.len(),
            started.elapsed()
        );

        for project in &found {
            println!(
                "  {:<12} {:<40} {}",
                project.format().to_string(),
                project.name(),
                discovery::read_headline(project).unwrap_or_default()
            );
        }
    }
}
