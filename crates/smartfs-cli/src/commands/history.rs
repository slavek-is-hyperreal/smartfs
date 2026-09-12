//! Handler for `history` command.

use smartfs_db::{version_history, FileVersionRecord, PgPool};
use smartfs_schema::error::Result;

use crate::args::HistoryArgs;
use crate::path::resolve_path;

/// @id: e6a1b2c3-3003-4000-8000-000000000001
/// Result of the `history` command.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryResult {
    pub path: String,
    pub versions: Vec<FileVersionRecord>,
}

/// @id: e6a1b2c3-3003-4000-8000-000000000002
/// Handles execution of the `history` command.
pub async fn handle_history(
    pool: &PgPool,
    args: &HistoryArgs,
) -> Result<HistoryResult> {
    let inode = resolve_path(pool, &args.path).await?;
    let versions = version_history(pool, inode.id).await?;

    Ok(HistoryResult {
        path: args.path.clone(),
        versions,
    })
}
