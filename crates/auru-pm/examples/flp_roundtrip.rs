//! Prove that a real `.flp` survives decode and re-encode byte for byte.
//!
//! The event stream has no length prefixes and no way to skip an event without
//! understanding its identifier band, so "it parsed without erroring" means
//! very little — a desynchronised read produces plausible nonsense. Comparing
//! the re-encoded bytes against the original is the only check that actually
//! demonstrates the parse was correct.
//!
//! ```text
//! cargo run --example flp_roundtrip -- "/path/to/Project.flp"
//! ```

use std::path::PathBuf;

use auru_pm::flstudio::events::Stream;

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: flp_roundtrip <project.flp>...");
        std::process::exit(2);
    }

    let mut failures = 0;
    for path in &paths {
        let source = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failures += 1;
                continue;
            }
        };

        match Stream::decode(&source) {
            Ok(stream) => {
                let encoded = stream.encode();
                let exact = encoded == source;
                println!("{}", path.display());
                println!(
                    "  {} bytes · {} events · ppq {} · {} channels · {}",
                    source.len(),
                    stream.events.len(),
                    stream.header.ppq,
                    stream.header.channels,
                    stream
                        .version()
                        .unwrap_or_else(|| "unknown version".to_owned()),
                );
                // The stricter check: through the canonical tree, which is
                // what a commit actually stores and a restore rebuilds from.
                let through_tree = auru_pm::flstudio::normalize_bytes(&source);
                match &through_tree {
                    Ok(bytes) if *bytes == source => {
                        println!("  via canonical tree: byte-exact")
                    }
                    Ok(bytes) => {
                        println!(
                            "  via canonical tree: DIFFERS ({} bytes out{})",
                            bytes.len(),
                            first_difference(&source, bytes)
                                .map(|at| format!(", first at {at}"))
                                .unwrap_or_default()
                        );
                        failures += 1;
                    }
                    Err(error) => {
                        println!("  via canonical tree: {error}");
                        failures += 1;
                    }
                }

                // And through a real commit, which additionally redacts.
                match auru_pm::ProjectSnapshot::from_source_bytes(
                    auru_pm::ProjectFormat::FlStudio,
                    &source,
                )
                .and_then(|snapshot| snapshot.restore_bytes())
                {
                    Ok(bytes) if bytes == source => {
                        println!("  via commit path: byte-exact (nothing to redact)")
                    }
                    Ok(bytes) if bytes.len() == source.len() => println!(
                        "  via commit path: identical length, differs at byte {} (redaction)",
                        first_difference(&source, &bytes).unwrap_or(0)
                    ),
                    Ok(bytes) => println!(
                        "  via commit path: {} bytes out vs {} in (redaction shortened it)",
                        bytes.len(),
                        source.len()
                    ),
                    Err(error) => {
                        println!("  via commit path: {error}");
                        failures += 1;
                    }
                }

                if exact {
                    println!("  round trip: byte-exact");
                } else {
                    println!(
                        "  round trip: DIFFERS ({} bytes in, {} out)",
                        source.len(),
                        encoded.len()
                    );
                    if let Some(at) = first_difference(&source, &encoded) {
                        println!("  first difference at byte {at}");
                    }
                    failures += 1;
                }
            }
            Err(error) => {
                println!("{}: {error}", path.display());
                failures += 1;
            }
        }
    }

    if failures > 0 {
        std::process::exit(1);
    }
}

fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    left.iter().zip(right).position(|(a, b)| a != b)
}
