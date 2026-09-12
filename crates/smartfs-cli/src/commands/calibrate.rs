//! Handler for `calibrate` command.

use smartfs_db::PgPool;
use smartfs_schema::error::Result;
use uuid::Uuid;

use crate::args::CalibrateArgs;

/// @id: e6a1b2c3-3008-4000-8000-000000000001
/// Result of the `calibrate` command.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrateResult {
    pub plugin_type: String,
    pub model_id: Uuid,
    pub join_threshold: f64,
}

/// @id: e6a1b2c3-3008-4000-8000-000000000002
/// Handles execution of the `calibrate` command.
pub async fn handle_calibrate(
    pool: &PgPool,
    args: &CalibrateArgs,
) -> Result<CalibrateResult> {
    let target = args.target_percentile.unwrap_or(0.15);
    let calibrated = smartfs_semantic::calibrate_join_threshold(
        pool,
        &args.plugin_type,
        args.model_id,
        target,
    )
    .await?;

    Ok(CalibrateResult {
        plugin_type: args.plugin_type.clone(),
        model_id: args.model_id,
        join_threshold: calibrated,
    })
}
