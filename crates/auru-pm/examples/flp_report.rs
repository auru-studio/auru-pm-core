//! Describe an FL Studio project: what it is, what it needs, what is at risk.
//!
//! ```text
//! cargo run --example flp_report -- "/path/to/Project.flp"
//! ```

use auru_pm::flstudio;
use std::path::Path;

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: flp_report <project.flp>...");
        std::process::exit(2);
    }

    for path in &paths {
        let source = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("{path}: {error}");
                continue;
            }
        };
        let meta = match flstudio::read_metadata(&source) {
            Ok(meta) => meta,
            Err(error) => {
                eprintln!("{path}: {error}");
                continue;
            }
        };

        println!("\n{path}");
        println!("  {}", meta.headline());
        if let Some(title) = &meta.title {
            println!("  title    {title}");
        }
        for (label, value) in [
            ("author", &meta.author),
            ("genre", &meta.genre),
            ("url", &meta.url),
        ] {
            if let Some(value) = value {
                println!("  {label:<8} {value}");
            }
        }
        println!(
            "  saved by FL {} (build {})",
            meta.version.as_deref().unwrap_or("?"),
            meta.build.map_or_else(|| "?".to_owned(), |b| b.to_string())
        );
        println!("  ppq      {}", meta.ppq);

        if !meta.markers.is_empty() {
            println!("  sections {}", meta.markers.join(" · "));
        }
        if !meta.insert_names.is_empty() {
            println!("  mixer    {}", meta.insert_names.join(" · "));
        }
        if !meta.pattern_names.is_empty() {
            println!("  patterns {}", meta.pattern_names.join(" · "));
        }

        let assets = &meta.assets;
        let plan =
            flstudio::plan_bundle_assets_from_directory(&source, Path::new(path).parent(), &[])
                .expect("metadata already proved the event stream is readable");
        println!(
            "\n  assets   {} distinct file(s) · {} need capture · {} found now",
            assets.total,
            assets.vendored(),
            plan.assets.len(),
        );
        if !plan.unresolved.is_empty() {
            println!(
                "    {:>4}  unresolved and omitted from a backup",
                plan.unresolved.len()
            );
        }
        for (label, count) in [
            ("in the project folder", assets.project_relative),
            ("elsewhere on this machine", assets.external),
            ("in the FL user folder", assets.user_data),
        ] {
            if count > 0 {
                println!("    {count:>4}  {label}");
            }
        }
        // The two worth saying out loud rather than tabulating.
        if assets.fragile > 0 {
            println!(
                "    {:>4}  AT RISK — in temporary space the system may delete",
                assets.fragile
            );
        }
        if assets.missing > 0 {
            println!("    {:>4}  no path recorded at all", assets.missing);
        }

        println!("\n  plugins  {}", meta.plugins.len());
        for plugin in &meta.plugins {
            let count = if plugin.instances > 1 {
                format!(" ×{}", plugin.instances)
            } else {
                String::new()
            };
            println!(
                "    {:<24} {:<9} {}{count}",
                plugin.name,
                plugin.format.label(),
                plugin.id
            );
        }
    }
}
