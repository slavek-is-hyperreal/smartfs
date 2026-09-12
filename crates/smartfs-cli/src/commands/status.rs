//! Handler for `status` command.

use smartfs_db::{count_all_unconsolidated, oldest_pending_age_secs, pending_backlog_count, PgPool};
use smartfs_schema::error::Result;

use crate::args::StatusArgs;

/// @id: e6a1b2c3-3009-4000-8000-000000000001
/// System status information.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemStatus {
    pub pending_backlog_count: i64,
    pub oldest_pending_age_secs: Option<f64>,
    pub unconsolidated_embedding_count: i64,
    pub calibrated_models_count: usize,
}

/// @id: e6a1b2c3-3009-4000-8000-000000000002
/// Handles execution of the `status` command.
pub async fn handle_status(
    pool: &PgPool,
    _args: &StatusArgs,
) -> Result<SystemStatus> {
    let backlog = pending_backlog_count(pool).await?;
    let oldest_age = oldest_pending_age_secs(pool).await?;
    let unconsolidated = count_all_unconsolidated(pool).await?;
    let combos = smartfs_semantic::fetch_calibrated_combinations(pool)
        .await
        .unwrap_or_default();

    Ok(SystemStatus {
        pending_backlog_count: backlog,
        oldest_pending_age_secs: oldest_age,
        unconsolidated_embedding_count: unconsolidated,
        calibrated_models_count: combos.len(),
    })
}
