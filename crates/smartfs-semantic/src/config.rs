//! Configuration loading and calibration for semantic consolidation.

use std::time::Duration;
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// @id: 1a7f3c02-9e44-4b1a-8f0a-2d6e5c9a7b11
/// Configuration loaded per `(plugin_type, model_id)` from `consolidation_thresholds`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationConfig {
    pub backlog_threshold: i64,
    pub idle_before_sleep: Duration,
    pub max_wait: Duration,
    pub join_threshold: f64,
    pub split_variance_threshold: f64,
    pub max_members_per_centroid: i64,
    pub batch_size: i64,
}

/// @id: 9c3e7a15-4b82-4d06-a9f1-3e8c5d2b7a44
/// Loads the consolidation configuration for a given `(plugin_type, model_id)` from the database.
///
/// Returns `Err(SmartFsError::MissingCalibration)` if no row is found.
pub async fn load_consolidation_config(
    db: &PgPool,
    plugin_type: &str,
    model_id: Uuid,
) -> Result<ConsolidationConfig> {
    let row = sqlx::query(
        r#"
        SELECT
            backlog_threshold,
            idle_before_sleep_secs,
            max_wait_secs,
            join_threshold,
            split_variance_threshold,
            max_members_per_centroid,
            batch_size
        FROM consolidation_thresholds
        WHERE plugin_type = $1 AND model_id = $2
        "#,
    )
    .bind(plugin_type)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .map_err(|e| SmartFsError::Db(format!("load_consolidation_config error: {e}")))?;

    let row = match row {
        Some(r) => r,
        None => {
            return Err(SmartFsError::MissingCalibration {
                plugin_type: plugin_type.to_string(),
                model_id,
            });
        }
    };

    let backlog_threshold: i64 = row
        .try_get("backlog_threshold")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let idle_before_sleep_secs: i32 = row
        .try_get("idle_before_sleep_secs")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let max_wait_secs: i32 = row
        .try_get("max_wait_secs")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let join_threshold: f64 = row
        .try_get("join_threshold")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let split_variance_threshold: f64 = row
        .try_get("split_variance_threshold")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let max_members_per_centroid: i64 = row
        .try_get("max_members_per_centroid")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    let batch_size: i32 = row
        .try_get("batch_size")
        .map_err(|e| SmartFsError::Db(e.to_string()))?;

    Ok(ConsolidationConfig {
        backlog_threshold,
        idle_before_sleep: Duration::from_secs(idle_before_sleep_secs.max(0) as u64),
        max_wait: Duration::from_secs(max_wait_secs.max(0) as u64),
        join_threshold,
        split_variance_threshold,
        max_members_per_centroid,
        batch_size: batch_size as i64,
    })
}

/// @id: 8b4c2e17-5f09-4d33-a6b1-9e2c4f8a5d30
/// Calibrates or updates the `join_threshold` for a `(plugin_type, model_id)` pair,
/// saving the row in `consolidation_thresholds`.
pub async fn calibrate_join_threshold(
    db: &PgPool,
    plugin_type: &str,
    model_id: Uuid,
    join_threshold: f64,
) -> Result<f64> {
    sqlx::query(
        r#"
        INSERT INTO consolidation_thresholds (
            plugin_type, model_id, join_threshold, calibrated_at, updated_at
        ) VALUES (
            $1, $2, $3, NOW(), NOW()
        )
        ON CONFLICT (plugin_type, model_id) DO UPDATE SET
            join_threshold = EXCLUDED.join_threshold,
            calibrated_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(plugin_type)
    .bind(model_id)
    .bind(join_threshold)
    .execute(db)
    .await
    .map_err(|e| SmartFsError::Db(format!("calibrate_join_threshold error: {e}")))?;

    Ok(join_threshold)
}
