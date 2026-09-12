//! Command-line binary for `smartfs-docgen`.

use std::path::PathBuf;
use std::process::ExitCode;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use smartfs_docgen::{
    backfill_missing_ids, check_registry_consistency, generate_registry,
    resolve_symbol_link, ConsistencyIssue, ResolvedLocation, SymbolRegistry,
};

#[derive(Parser, Debug)]
#[command(
    name = "smartfs-docgen",
    about = "SmartFS Symbol Registry Generator and Link Resolver",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scans crates and outputs formatted symbol registry JSON to stdout.
    Scan {
        /// Directory containing crates (e.g. ./crates).
        crates_dir: PathBuf,
    },
    /// Executes idempotent codemod backfilling missing UUIDs in source comments.
    Backfill {
        /// Directory containing crates.
        crates_dir: PathBuf,
    },
    /// Runs CI consistency checks across crates and documentation.
    Check {
        /// Directory containing crates.
        crates_dir: PathBuf,
        /// Directory containing markdown documentation.
        docs_dir: PathBuf,
    },
    /// Resolves a symbol UUID to its current source file and line.
    Resolve {
        /// Symbol UUID to resolve.
        id: Uuid,
        /// Path to crates directory.
        #[arg(long, default_value = "crates")]
        crates_dir: PathBuf,
        /// Path to symbol registry JSON file.
        #[arg(long, default_value = "docs/symbol_registry.json")]
        registry_file: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan { crates_dir } => match generate_registry(&crates_dir) {
            Ok(registry) => match registry.to_json() {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("Error serializing registry: {err}");
                    ExitCode::FAILURE
                }
            },
            Err(err) => {
                eprintln!("Error scanning crates: {err}");
                ExitCode::FAILURE
            }
        },
        Commands::Backfill { crates_dir } => match backfill_missing_ids(&crates_dir) {
            Ok(count) => {
                println!("Backfilled {count} missing symbol IDs.");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("Error backfilling IDs: {err}");
                ExitCode::FAILURE
            }
        },
        Commands::Check {
            crates_dir,
            docs_dir,
        } => match check_registry_consistency(&crates_dir, &docs_dir) {
            Ok(issues) => {
                if issues.is_empty() {
                    println!("Consistency check passed: 0 issues found.");
                    ExitCode::SUCCESS
                } else {
                    eprintln!("Consistency check failed with {} issue(s):", issues.len());
                    for issue in &issues {
                        match issue {
                            ConsistencyIssue::MissingId {
                                file,
                                line,
                                symbol_name,
                            } => {
                                eprintln!(
                                    "  [MissingId] {}:{}: symbol `{symbol_name}` missing @id",
                                    file.display(),
                                    line
                                );
                            }
                            ConsistencyIssue::DuplicateId { id, locations } => {
                                eprintln!("  [DuplicateId] {id} duplicated across:");
                                for (f, l) in locations {
                                    eprintln!("    - {}:{}", f.display(), l);
                                }
                            }
                            ConsistencyIssue::DeadLink { doc_file, id } => {
                                eprintln!(
                                    "  [DeadLink] {}: unresolved or tombstoned link `symbol://{id}`",
                                    doc_file.display()
                                );
                            }
                            ConsistencyIssue::StaleCache {
                                id,
                                expected,
                                actual,
                            } => {
                                eprintln!(
                                    "  [StaleCache] {id}: registry has {}:{}, actual is {}:{}",
                                    expected.0.display(),
                                    expected.1,
                                    actual.0.display(),
                                    actual.1
                                );
                            }
                        }
                    }
                    ExitCode::FAILURE
                }
            }
            Err(err) => {
                eprintln!("Error running consistency check: {err}");
                ExitCode::FAILURE
            }
        },
        Commands::Resolve {
            id,
            crates_dir,
            registry_file,
        } => {
            let registry = if registry_file.is_file() {
                match std::fs::read_to_string(&registry_file)
                    .map_err(Into::into)
                    .and_then(|s| SymbolRegistry::from_json(&s))
                {
                    Ok(reg) => reg,
                    Err(err) => {
                        eprintln!(
                            "Warning: failed reading registry at {}: {err}",
                            registry_file.display()
                        );
                        match generate_registry(&crates_dir) {
                            Ok(reg) => reg,
                            Err(e) => {
                                eprintln!("Error generating registry: {e}");
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                }
            } else {
                match generate_registry(&crates_dir) {
                    Ok(reg) => reg,
                    Err(e) => {
                        eprintln!("Error generating registry: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            };

            match resolve_symbol_link(&registry, id, &crates_dir) {
                Ok(ResolvedLocation::Found { file, line }) => {
                    println!("{}:{}", file.display(), line);
                    ExitCode::SUCCESS
                }
                Ok(ResolvedLocation::Tombstoned { removed_summary }) => {
                    let summary = removed_summary
                        .unwrap_or_else(|| "No doc summary available".to_string());
                    eprintln!("Symbol {id} is tombstoned (removed): {summary}");
                    ExitCode::FAILURE
                }
                Err(err) => {
                    eprintln!("Failed to resolve symbol {id}: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
