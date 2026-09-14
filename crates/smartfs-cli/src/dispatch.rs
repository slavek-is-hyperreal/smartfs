//! Command dispatcher for `smartfs-cli`.

use std::io::Write;
use smartfs_db::PgPool;
use smartfs_schema::error::Result;
use smartfs_store::{BlobStore, LocalDiskStore};

use crate::args::{Cli, Commands};
use crate::commands::*;

/// @id: e6a1b2c3-4001-4000-8000-000000000001
/// Dispatches and executes a CLI command against the provided database pool and blob store.
pub async fn dispatch_command(
    pool: &PgPool,
    store: &dyn BlobStore,
    store_path: &std::path::Path,
    command: &Commands,
) -> Result<String> {
    match command {
        Commands::Write(args) => {
            let res = handle_write(pool, store, args).await?;
            Ok(format!(
                "Successfully wrote '{}': version_id={}, blob_id={}, content_hash={}, size={} bytes",
                res.path, res.version_id, res.blob_id, res.content_hash, res.size
            ))
        }
        Commands::Cat(args) => {
            let res = handle_cat(pool, store, args).await?;
            match String::from_utf8(res.data) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("<Binary data: {} bytes>", e.into_bytes().len())),
            }
        }
        Commands::History(args) => {
            let res = handle_history(pool, args).await?;
            let mut out = format!("Version history for '{}' ({} versions):\n", res.path, res.versions.len());
            out.push_str(&format!(
                "{:<5} {:<14} {:<10} {:<64} {}\n",
                "VER", "STATUS", "SIZE", "HASH", "CREATED AT"
            ));
            out.push_str(&format!("{}\n", "-".repeat(110)));
            for v in &res.versions {
                out.push_str(&format!(
                    "{:<5} {:<14} {:<10} {:<64} {}\n",
                    v.version_number,
                    v.status,
                    v.size,
                    v.content_hash,
                    v.created_at.to_rfc3339()
                ));
            }
            Ok(out)
        }
        Commands::Search(args) => {
            let res = handle_search(pool, args).await?;
            let mut out = format!("Search results for '{}' ({} hits):\n", res.query, res.hits.len());
            for (idx, hit) in res.hits.iter().enumerate() {
                out.push_str(&format!(
                    "{}. [Score: {:.4}] [{:?}] ID: {}\n   Snippet: {}\n",
                    idx + 1,
                    hit.score,
                    hit.kind,
                    hit.id,
                    hit.snippet.trim()
                ));
            }
            Ok(out)
        }
        Commands::Diff(args) => {
            let res = handle_diff(pool, args).await?;
            let mut out = format!(
                "AST node diff for '{}' between version {} and {}:\n",
                res.path, res.v1, res.v2
            );
            out.push_str(&format!("Added ({})\n", res.added.len()));
            for n in &res.added {
                out.push_str(&format!(
                    "  + [{}] {} (lines {}-{})\n",
                    n.kind, n.name, n.start_line, n.end_line
                ));
            }
            out.push_str(&format!("Changed ({})\n", res.changed.len()));
            for n in &res.changed {
                out.push_str(&format!(
                    "  ~ [{}] {} (lines {}-{})\n",
                    n.kind, n.name, n.start_line, n.end_line
                ));
            }
            out.push_str(&format!("Removed ({})\n", res.removed.len()));
            for n in &res.removed {
                out.push_str(&format!(
                    "  - [{}] {} (lines {}-{})\n",
                    n.kind, n.name, n.start_line, n.end_line
                ));
            }
            Ok(out)
        }
        Commands::Import(args) => {
            let res = handle_import(pool, store, args).await?;
            Ok(format!(
                "Import completed for '{}' (mode: {:?}):\n  Directories imported: {}\n  Files imported: {}\n  Total bytes: {}",
                res.root_dir.display(),
                res.mode,
                res.directories_imported,
                res.files_imported,
                res.total_bytes
            ))
        }
        Commands::Concepts(args) => {
            let res = handle_concepts(pool, args).await?;
            let mut out = format!("Active concept centroids ({} configurations):\n", res.concepts.len());
            out.push_str(&format!(
                "{:<16} {:<38} {:<12} {:<10} {}\n",
                "PLUGIN", "MODEL ID", "JOIN THRESH", "BACKLOG", "UNCONSOLIDATED"
            ));
            out.push_str(&format!("{}\n", "-".repeat(95)));
            for c in &res.concepts {
                out.push_str(&format!(
                    "{:<16} {:<38} {:<12.4} {:<10} {}\n",
                    c.plugin_type,
                    c.model_id,
                    c.join_threshold,
                    c.backlog_threshold,
                    c.unconsolidated_count
                ));
            }
            Ok(out)
        }
        Commands::Calibrate(args) => {
            let res = handle_calibrate(pool, args).await?;
            Ok(format!(
                "Successfully calibrated plugin '{}' and model '{}': join_threshold = {:.4}",
                res.plugin_type, res.model_id, res.join_threshold
            ))
        }
        Commands::Status(args) => {
            let res = handle_status(pool, store_path, args).await?;
            let age_str = match res.oldest_pending_age_secs {
                Some(age) => format!("{:.1}s", age),
                None => "None (queue empty)".to_string(),
            };
            // ADR-58: the on-disk queue is reported separately from the
            // embedding backlog. They are different queues with different
            // failure modes, and merging them would hide either one.
            let queue_age = match res.pending_queue_oldest_age_secs {
                Some(age) => format!("{:.1}s", age),
                None => "None (queue empty)".to_string(),
            };
            Ok(format!(
                "SmartFS System Status:\n  \
                 Pending backlog count: {}\n  \
                 Oldest pending age:    {}\n  \
                 Unconsolidated embeds: {}\n  \
                 Calibrated models:     {}\n\
                 Write queue (ADR-58, on disk):\n  \
                 Uncommitted markers:   {}\n  \
                 Oldest marker age:     {}\n\
                 Blobs (ADR-62):\n  \
                 Private (fast delete): {}\n  \
                 Shared (deduplicated): {}",
                res.pending_backlog_count,
                age_str,
                res.unconsolidated_embedding_count,
                res.calibrated_models_count,
                res.pending_queue_depth,
                queue_age,
                res.private_blob_count,
                res.shared_blob_count
            ))
        }
        Commands::Recover(args) => {
            let res = handle_recover(pool, store_path, args).await?;
            match res.report {
                None => Ok(format!(
                    "Recovery dry run over {}:\n  {} uncommitted marker(s) queued. \
                     Re-run without --dry-run to commit them.",
                    res.queue_dir, res.found
                )),
                Some(r) => Ok(format!(
                    "Recovery pass over {}:\n  \
                     Found:             {}\n  \
                     Committed now:     {}\n  \
                     Already committed: {}\n  \
                     Failed (left for retry): {}",
                    res.queue_dir, r.found, r.committed, r.already_committed, r.failed
                )),
            }
        }
    }
}

/// @id: e6a1b2c3-4001-4000-8000-000000000002
/// Runs the SmartFS CLI application with the given arguments.
pub async fn run_cli(cli: Cli) -> Result<()> {
    let pool = smartfs_db::connect_pool(&cli.database_url()).await?;
    let store_path = cli.store_path();
    let store = LocalDiskStore::new(&store_path);

    // For `cat`, write raw bytes to stdout directly
    if let Commands::Cat(ref args) = cli.command {
        let res = handle_cat(&pool, &store, args).await?;
        std::io::stdout().write_all(&res.data)?;
        return Ok(());
    }

    let output = dispatch_command(&pool, &store, &store_path, &cli.command).await?;
    println!("{output}");
    Ok(())
}
