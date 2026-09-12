use crate::activity::ActivityMonitor;
use crate::engine::EmbeddingEngine;
use smartfs_db::{get_ast_nodes, version_get_by_id, PgPool};
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::BlobStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

/// @id: 34def012-789a-5bcd-3e45-6789abcdef01
/// Check anti-starvation valve criteria (ADR-42):
/// Return true if oldest pending > 60s OR pending backlog count > 1000.
pub async fn should_embed_despite_activity(pool: &PgPool) -> bool {
    let oldest_age = smartfs_db::oldest_pending_age_secs(pool)
        .await
        .unwrap_or(None)
        .unwrap_or(0.0);
    if oldest_age > 60.0 {
        return true;
    }

    let backlog = smartfs_db::pending_backlog_count(pool)
        .await
        .unwrap_or(0);
    if backlog > 1000 {
        return true;
    }

    false
}

/// @id: 45ef0123-89ab-6cde-4f56-789abcdef012
/// Generate and store embeddings for a specific file version, including:
/// - Filling `file_versions.search_text` (ADR-54)
/// - Embedding generic file content into `embeddings_1024_qwen` (ADR-49)
/// - Embedding AST nodes into `ast_embeddings_1536` if AST parsing was active
pub async fn embed_version(
    pool: &PgPool,
    store: &dyn BlobStore,
    engine: &dyn EmbeddingEngine,
    version_id: Uuid,
    default_model_id: Uuid,
) -> Result<()> {
    // 1. Retrieve file version metadata
    let version = version_get_by_id(pool, version_id)
        .await?
        .ok_or_else(|| SmartFsError::NotFound(format!("Version {version_id} not found")))?;

    // 2. Obtain raw content from store (if blob exists)
    let content_bytes = if let Some(blob_id) = version.blob_id {
        store.get(blob_id, version.external_path.as_deref()).await?
    } else {
        Vec::new()
    };

    // 3. Determine search_text and embedding text (ADR-54)
    // Decompress if content was zstd-compressed in store
    let raw_bytes = if !content_bytes.is_empty() {
        zstd::decode_all(content_bytes.as_slice()).unwrap_or(content_bytes)
    } else {
        content_bytes
    };

    let (search_text, text_for_embedding) = match String::from_utf8(raw_bytes) {
        Ok(valid_utf8) => {
            let text = valid_utf8;
            (Some(text.clone()), text)
        }
        Err(_) => {
            // Binary file fallback: concatenate string fields from special_data JSON (ADR-54 §Context)
            let mut extracted = Vec::new();
            extract_strings_from_json(&version.special_data, &mut extracted);
            let combined = if extracted.is_empty() {
                None
            } else {
                Some(extracted.join(" "))
            };
            let text = combined.clone().unwrap_or_default();
            (combined, text)
        }
    };

    // Store search_text in file_versions
    smartfs_db::set_search_text(pool, version_id, search_text.as_deref()).await?;

    // 4. Generate and insert file-level embedding (Qwen3 1024-dim, ADR-49)
    let vector_1024 = engine.embed(&text_for_embedding, 1024).await?;
    smartfs_db::insert_embedding_1024_qwen(
        pool,
        version_id,
        default_model_id,
        &version.special_type,
        &vector_1024,
    )
    .await?;

    // 5. Generate and insert AST node embeddings if AST nodes exist for this version
    let ast_nodes = get_ast_nodes(pool, version_id).await?;
    for node in ast_nodes {
        let node_vector = engine.embed(&node.source, 1536).await?;
        smartfs_db::insert_ast_embedding_1536(
            pool,
            node.id,
            default_model_id,
            &version.special_type,
            &node_vector,
        )
        .await?;
    }

    Ok(())
}

/// @id: 56f01234-9abc-7def-5a67-89abcdef0123
/// Handle the outcome of an embedding pass without bubbling errors (ADR-34).
/// On success: mark_clean + refresh_is_current (FIX-02).
/// On error: log and mark_failed (increments retry_count).
pub async fn finish_embed(pool: &PgPool, version_id: Uuid, result: Result<()>) {
    match result {
        Ok(()) => {
            if let Err(e) = smartfs_db::mark_clean(pool, version_id).await {
                tracing::error!("Failed to mark version {version_id} clean: {e}");
            }
            if let Err(e) = smartfs_db::refresh_is_current(pool, version_id).await {
                tracing::error!("Failed to refresh is_current for version {version_id}: {e}");
            }
        }
        Err(e) => {
            tracing::error!("Embedding failed for version {version_id}: {e}");
            if let Err(mark_err) = smartfs_db::mark_failed(pool, version_id).await {
                tracing::error!("Failed to mark version {version_id} failed: {mark_err}");
            }
        }
    }
}

/// @id: 67012345-abcd-8ef0-6b78-9abcdef01234
/// Run the background embedding supervisor loop (supervisor — never exits).
/// Implements anti-starvation valve (ADR-42) and non-preemptible first batch item in force mode (FIX-05, ADR-47).
pub async fn run_worker_supervisor(
    pool: PgPool,
    store: Arc<dyn BlobStore>,
    engine: Arc<dyn EmbeddingEngine>,
    activity_monitor: Arc<ActivityMonitor>,
    batch_size: i64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Startup reaper (§3.5): reset stale processing rows back to pending
    if let Err(e) = smartfs_db::reaper(&pool).await {
        tracing::error!("Startup reaper failed: {e}");
    }

    let default_model_id = match smartfs_db::get_default_model_id(&pool).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to lookup default embedding model ID: {e}");
            return;
        }
    };

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        // Anti-starvation valve check (ADR-42)
        let force = should_embed_despite_activity(&pool).await;

        if !force {
            tokio::select! {
                _ = activity_monitor.wait_for_idle(Duration::from_millis(500)) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() { break; }
                }
            }
        }

        let batch = match smartfs_db::claim_pending_to_processing(&pool, batch_size).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("claim_pending_to_processing failed: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        if batch.is_empty() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() { break; }
                }
            }
            continue;
        }

        let mut preempted = false;
        let mut preempt_idx = batch.len();

        for (i, version_id) in batch.iter().enumerate() {
            if *shutdown_rx.borrow() {
                preempted = true;
                preempt_idx = i;
                break;
            }

            // In force mode, first item in batch is NON-PREEMPTIBLE (FIX-05, ADR-47)
            let allow_preempt = !force || i > 0;

            if allow_preempt {
                tokio::select! {
                    res = embed_version(&pool, store.as_ref(), engine.as_ref(), *version_id, default_model_id) => {
                        finish_embed(&pool, *version_id, res).await;
                    }
                    _ = activity_monitor.write_activity_detected() => {
                        preempted = true;
                        preempt_idx = i;
                        break;
                    }
                }
            } else {
                let res = embed_version(&pool, store.as_ref(), engine.as_ref(), *version_id, default_model_id).await;
                finish_embed(&pool, *version_id, res).await;
            }
        }

        if preempted {
            for version_id in &batch[preempt_idx..] {
                let _ = smartfs_db::revert_to_pending(&pool, *version_id).await;
            }
        }
    }
}

fn extract_strings_from_json(val: &serde_json::Value, out: &mut Vec<String>) {
    match val {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_strings_from_json(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map {
                extract_strings_from_json(v, out);
            }
        }
        _ => {}
    }
}
