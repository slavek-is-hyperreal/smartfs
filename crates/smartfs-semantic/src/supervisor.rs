//! Supervisor topology, advisory locking, and background worker loop for consolidation.

use crate::config::load_consolidation_config;
use crate::consolidation::{consolidate_batch, count_unconsolidated, merge_centroids};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use uuid::Uuid;

fn rot(x: u32, k: u32) -> u32 {
    x.rotate_left(k)
}

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= rot(*c, 4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= rot(*a, 6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= rot(*b, 8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= rot(*c, 16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= rot(*a, 19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= rot(*b, 4);
    *b = b.wrapping_add(*a);
}

fn final_mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 14));
    *a ^= *c;
    *a = a.wrapping_sub(rot(*c, 11));
    *b ^= *a;
    *b = b.wrapping_sub(rot(*a, 25));
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 16));
    *a ^= *c;
    *a = a.wrapping_sub(rot(*c, 4));
    *b ^= *a;
    *b = b.wrapping_sub(rot(*a, 14));
    *c ^= *b;
    *c = c.wrapping_sub(rot(*b, 24));
}

/// @id: 85678901-9012-4b23-3456-789012345678
/// Computes the exact 32-bit Bob Jenkins hash equivalent to PostgreSQL's `hashtext()`.
pub fn pg_hash_bytes(k: &[u8]) -> i32 {
    let mut len = k.len();
    let mut a: u32 = 0x9e3779b9_u32
        .wrapping_add(len as u32)
        .wrapping_add(3923095);
    let mut b: u32 = a;
    let mut c: u32 = a;

    let mut offset = 0;
    while len >= 12 {
        a = a.wrapping_add(
            (k[offset] as u32)
                | ((k[offset + 1] as u32) << 8)
                | ((k[offset + 2] as u32) << 16)
                | ((k[offset + 3] as u32) << 24),
        );
        b = b.wrapping_add(
            (k[offset + 4] as u32)
                | ((k[offset + 5] as u32) << 8)
                | ((k[offset + 6] as u32) << 16)
                | ((k[offset + 7] as u32) << 24),
        );
        c = c.wrapping_add(
            (k[offset + 8] as u32)
                | ((k[offset + 9] as u32) << 8)
                | ((k[offset + 10] as u32) << 16)
                | ((k[offset + 11] as u32) << 24),
        );
        mix(&mut a, &mut b, &mut c);
        offset += 12;
        len -= 12;
    }

    if len == 11 {
        c = c.wrapping_add((k[offset + 10] as u32) << 24);
    }
    if len >= 10 {
        c = c.wrapping_add((k[offset + 9] as u32) << 16);
    }
    if len >= 9 {
        c = c.wrapping_add((k[offset + 8] as u32) << 8);
    }
    if len >= 8 {
        b = b.wrapping_add((k[offset + 7] as u32) << 24);
    }
    if len >= 7 {
        b = b.wrapping_add((k[offset + 6] as u32) << 16);
    }
    if len >= 6 {
        b = b.wrapping_add((k[offset + 5] as u32) << 8);
    }
    if len >= 5 {
        b = b.wrapping_add(k[offset + 4] as u32);
    }
    if len >= 4 {
        a = a.wrapping_add((k[offset + 3] as u32) << 24);
    }
    if len >= 3 {
        a = a.wrapping_add((k[offset + 2] as u32) << 16);
    }
    if len >= 2 {
        a = a.wrapping_add((k[offset + 1] as u32) << 8);
    }
    if len >= 1 {
        a = a.wrapping_add(k[offset] as u32);
    }

    final_mix(&mut a, &mut b, &mut c);
    c as i32
}

/// @id: 63f4b810-1c92-4d55-87a3-e29b4c7d0a11
/// Computes the deterministic 64-bit advisory lock key matching `hashtext(plugin_type || model_id)` in PostgreSQL.
pub fn advisory_lock_key(plugin_type: &str, model_id: Uuid) -> i64 {
    let key_str = format!("{plugin_type}{model_id}");
    pg_hash_bytes(key_str.as_bytes()) as i64
}

/// @id: 7f3b8c19-2e04-4d81-8b22-6c5d7a9f1e03
/// Attempts to acquire a transactionless session-level PostgreSQL advisory lock.
pub async fn try_advisory_lock(db: &PgPool, lock_key: i64) -> Result<bool> {
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lock_key)
        .fetch_one(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("try_advisory_lock error: {e}")))?;
    Ok(acquired)
}

/// @id: 5a9c1d83-4e72-4b60-9d11-3f8a2c7b5e02
/// Releases a session-level PostgreSQL advisory lock.
pub async fn release_advisory_lock(db: &PgPool, lock_key: i64) -> Result<()> {
    let _released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(lock_key)
        .fetch_one(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("release_advisory_lock error: {e}")))?;
    Ok(())
}

/// @id: a1f8e234-9c01-4b77-8d55-1e3c5a7f9b12
/// Fetches all calibrated `(plugin_type, model_id)` pairs from `consolidation_thresholds`.
pub async fn fetch_calibrated_combinations(db: &PgPool) -> Result<Vec<(String, Uuid)>> {
    let rows = sqlx::query("SELECT DISTINCT plugin_type, model_id FROM consolidation_thresholds")
        .fetch_all(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("fetch_calibrated_combinations error: {e}")))?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let plugin_type: String = row
            .try_get("plugin_type")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let model_id: Uuid = row
            .try_get("model_id")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        result.push((plugin_type, model_id));
    }
    Ok(result)
}

/// @id: 0f4a8e21-6d53-4b19-9c72-1a5e8d3f6b04
/// Discovers all calibrated combinations in `consolidation_thresholds` and maintains
/// one supervisor tokio task per combination, polling periodically for new combinations.
pub async fn spawn_all_consolidation_supervisors(db: PgPool) -> Result<()> {
    let mut running: HashMap<(String, Uuid), JoinHandle<()>> = HashMap::new();
    const REDISCOVER_INTERVAL: Duration = Duration::from_secs(600);

    loop {
        let combos = fetch_calibrated_combinations(&db).await?;
        for combo in combos {
            running.entry(combo.clone()).or_insert_with(|| {
                let db_clone = db.clone();
                let combo_clone = combo.clone();
                tokio::spawn(async move {
                    consolidation_supervisor(db_clone, combo_clone).await;
                })
            });
        }
        tokio::time::sleep(REDISCOVER_INTERVAL).await;
    }
}

/// @id: 6b2d4e18-3f77-4a90-9c11-8a5f0d2e7c44
/// Supervisor loop for a single `(plugin_type, model_id)` pair.
///
/// Operates two timers (backlog/idle and max_wait) guarded by `pg_try_advisory_lock`.
/// Periodically runs `merge_centroids` as a backstop against slow drift.
pub async fn consolidation_supervisor(db: PgPool, combo: (String, Uuid)) {
    let (plugin_type, model_id) = combo;
    let mut last_run = Instant::now();
    let mut cycles_since_merge = 0u32;
    const MERGE_CHECK_INTERVAL_CYCLES: u32 = 20;

    loop {
        let cfg = match load_consolidation_config(&db, &plugin_type, model_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("no consolidation config for {plugin_type}/{model_id}: {e}");
                return;
            }
        };

        let backlog = count_unconsolidated(&db, &plugin_type, model_id)
            .await
            .unwrap_or(0);

        let should_run = backlog >= cfg.backlog_threshold
            || last_run.elapsed() >= cfg.max_wait
            || (backlog > 0 && last_run.elapsed() >= cfg.idle_before_sleep);

        if should_run {
            let lock_key = advisory_lock_key(&plugin_type, model_id);
            if try_advisory_lock(&db, lock_key).await.unwrap_or(false) {
                match consolidate_batch(&db, &plugin_type, model_id, &cfg).await {
                    Ok(n) => {
                        tracing::info!("consolidated {n} vectors for {plugin_type}/{model_id}");
                        last_run = Instant::now();
                        cycles_since_merge += 1;

                        if cycles_since_merge >= MERGE_CHECK_INTERVAL_CYCLES {
                            cycles_since_merge = 0;
                            let merge_thresh = cfg.join_threshold / 2.0;
                            if let Ok(mut tx) = db.begin().await {
                                match merge_centroids(&mut tx, &plugin_type, model_id, merge_thresh)
                                    .await
                                {
                                    Ok(merged_count) => {
                                        if let Err(e) = tx.commit().await {
                                            tracing::error!("failed to commit merge_centroids: {e}");
                                        } else if merged_count > 0 {
                                            tracing::info!(
                                                "merged {merged_count} centroid pairs for {plugin_type}/{model_id}"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("merge_centroids failed: {e}");
                                        let _ = tx.rollback().await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => tracing::error!("consolidation batch failed: {e}"),
                }
                release_advisory_lock(&db, lock_key).await.ok();
            }
        } else {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
}
