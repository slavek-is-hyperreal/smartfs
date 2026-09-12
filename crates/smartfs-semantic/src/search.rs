//! Hybrid concept search merging crystallized centroid routing and working memory buffer.

use serde::{Deserialize, Serialize};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

use crate::types::CentroidSummary;

/// @id: 5e9b1023-4182-4f33-8a02-1b9c7d4e5f60
/// Source layer of a concept search hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitSource {
    Crystallized,
    Buffered,
}

/// @id: 6f0a2134-5293-5044-9b13-2cae8e5f6071
/// Search result from concept search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConceptSearchHit {
    pub id: Uuid,
    pub distance: f64,
    pub source: HitSource,
}

fn format_vector(vector: &[f32]) -> String {
    let mut s = String::with_capacity(vector.len() * 12 + 2);
    s.push('[');
    for (i, v) in vector.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&v.to_string());
    }
    s.push(']');
    s
}

async fn resolve_tables(
    db: &PgPool,
    model_id: Uuid,
    vector_len: usize,
) -> Result<(&'static str, &'static str, &'static str, &'static str, bool)> {
    let dim_opt: Option<i32> = sqlx::query_scalar(
        "SELECT dimensions FROM embedding_models WHERE id = $1",
    )
    .bind(model_id)
    .fetch_optional(db)
    .await
    .map_err(|e| SmartFsError::Db(format!("resolve_tables model fetch error: {e}")))?;

    let dim = match dim_opt {
        Some(d) => d as usize,
        None => vector_len,
    };

    match dim {
        1536 => Ok((
            "concept_centroids_1536",
            "centroid_members_1536",
            "ast_embeddings_1536",
            "ast_node_id",
            true,
        )),
        1024 => Ok((
            "concept_centroids_1024_qwen",
            "centroid_members_1024_qwen",
            "embeddings_1024_qwen",
            "version_id",
            false,
        )),
        768 => Ok((
            "concept_centroids_768",
            "centroid_members_768",
            "embeddings_768",
            "version_id",
            false,
        )),
        384 => Ok((
            "concept_centroids_384",
            "centroid_members_384",
            "embeddings_384",
            "version_id",
            false,
        )),
        other => Err(SmartFsError::Other(format!(
            "Unsupported dimension {other} for model {model_id}"
        ))),
    }
}

/// @id: 7d1f4b29-6a83-4c05-9e17-2b8d5f0a3c66
/// Unified concept search that queries both crystallized memory (via centroids)
/// and unconsolidated working memory (buffer), merging and deduplicating results.
pub async fn search_by_concept(
    db: &PgPool,
    query_vector: &[f32],
    plugin_type: &str,
    model_id: Uuid,
    limit: usize,
) -> Result<Vec<ConceptSearchHit>> {
    let crystallized = search_via_centroids(db, query_vector, plugin_type, model_id, limit).await?;
    let buffered = brute_force_unconsolidated(db, query_vector, plugin_type, model_id, limit).await?;
    Ok(merge_by_similarity(crystallized, buffered, limit))
}

/// @id: 8e205c3a-7b94-4d16-af28-3c9d6e1b4a77
/// Searches crystallized memory using IVF-style routing:
/// 1. Finds nearest active centroids to query vector.
/// 2. Searches candidate members of those centroids.
pub async fn search_via_centroids(
    db: &PgPool,
    query_vector: &[f32],
    plugin_type: &str,
    model_id: Uuid,
    limit: usize,
) -> Result<Vec<ConceptSearchHit>> {
    let (c_tbl, m_tbl, e_tbl, id_col, is_ast) =
        resolve_tables(db, model_id, query_vector.len()).await?;
    let vec_str = format_vector(query_vector);
    let limit_i64 = limit as i64;

    let sql = if is_ast {
        format!(
            r#"
            WITH nearest_centroids AS (
                SELECT id
                FROM {c_tbl}
                WHERE is_active = TRUE
                  AND plugin_type = $1
                  AND model_id = $2
                ORDER BY centroid <=> $3::vector ASC
                LIMIT 10
            )
            SELECT
                cm.{id_col} AS id,
                (ae.embedding <=> $3::vector)::float8 AS distance
            FROM nearest_centroids nc
            JOIN {m_tbl} cm ON nc.id = cm.centroid_id
            JOIN {e_tbl} ae ON cm.ast_node_id = ae.ast_node_id
            WHERE ae.is_current = TRUE
            ORDER BY distance ASC
            LIMIT $4
            "#
        )
    } else {
        format!(
            r#"
            WITH nearest_centroids AS (
                SELECT id
                FROM {c_tbl}
                WHERE is_active = TRUE
                  AND plugin_type = $1
                  AND model_id = $2
                ORDER BY centroid <=> $3::vector ASC
                LIMIT 10
            )
            SELECT
                cm.{id_col} AS id,
                (e.embedding <=> $3::vector)::float8 AS distance
            FROM nearest_centroids nc
            JOIN {m_tbl} cm ON nc.id = cm.centroid_id
            JOIN {e_tbl} e ON cm.version_id = e.version_id
            ORDER BY distance ASC
            LIMIT $4
            "#
        )
    };

    let rows = sqlx::query(&sql)
        .bind(plugin_type)
        .bind(model_id)
        .bind(&vec_str)
        .bind(limit_i64)
        .fetch_all(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("search_via_centroids error: {e}")))?;

    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let distance: f64 = row
            .try_get("distance")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        hits.push(ConceptSearchHit {
            id,
            distance,
            source: HitSource::Crystallized,
        });
    }

    Ok(hits)
}

/// @id: 9f316d4b-8ca5-4e27-b039-4dae7f2c5b88
/// Performs a brute-force scan over unconsolidated vectors in working memory (`consolidated = FALSE`).
pub async fn brute_force_unconsolidated(
    db: &PgPool,
    query_vector: &[f32],
    plugin_type: &str,
    model_id: Uuid,
    limit: usize,
) -> Result<Vec<ConceptSearchHit>> {
    let (_, _, e_tbl, id_col, is_ast) =
        resolve_tables(db, model_id, query_vector.len()).await?;
    let vec_str = format_vector(query_vector);
    let limit_i64 = limit as i64;

    let sql = if is_ast {
        format!(
            r#"
            SELECT
                {id_col} AS id,
                (embedding <=> $3::vector)::float8 AS distance
            FROM {e_tbl}
            WHERE consolidated = FALSE
              AND is_current = TRUE
              AND plugin_type = $1
              AND model_id = $2
            ORDER BY distance ASC
            LIMIT $4
            "#
        )
    } else {
        format!(
            r#"
            SELECT
                {id_col} AS id,
                (embedding <=> $3::vector)::float8 AS distance
            FROM {e_tbl}
            WHERE consolidated = FALSE
              AND plugin_type = $1
              AND model_id = $2
            ORDER BY distance ASC
            LIMIT $4
            "#
        )
    };

    let rows = sqlx::query(&sql)
        .bind(plugin_type)
        .bind(model_id)
        .bind(&vec_str)
        .bind(limit_i64)
        .fetch_all(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("brute_force_unconsolidated error: {e}")))?;

    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let distance: f64 = row
            .try_get("distance")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        hits.push(ConceptSearchHit {
            id,
            distance,
            source: HitSource::Buffered,
        });
    }

    Ok(hits)
}

/// @id: a0427e5c-9db6-4f38-c14a-5ebf8a3d6c99
/// Merges crystallized and buffered hits, deduplicating by ID (favoring smaller distance)
/// and sorting by distance ascending up to `limit`.
pub fn merge_by_similarity(
    crystallized: Vec<ConceptSearchHit>,
    buffered: Vec<ConceptSearchHit>,
    limit: usize,
) -> Vec<ConceptSearchHit> {
    let mut map: HashMap<Uuid, ConceptSearchHit> = HashMap::new();

    for hit in crystallized.into_iter().chain(buffered) {
        map.entry(hit.id)
            .and_modify(|existing| {
                if hit.distance < existing.distance {
                    *existing = hit.clone();
                }
            })
            .or_insert(hit);
    }

    let mut all: Vec<ConceptSearchHit> = map.into_values().collect();
    all.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(limit);
    all
}

/// @id: 4a2d8e19-3f05-4c92-b817-5e9d2c4b1a88
/// Lists active centroids across concept tables, optionally filtered by plugin_type and model_id,
/// ordered by member_count descending.
pub async fn list_active_centroids(
    db: &PgPool,
    plugin_type: Option<&str>,
    model_id: Option<Uuid>,
    limit: usize,
) -> Result<Vec<CentroidSummary>> {
    let mut summaries = Vec::new();
    let tables = [
        "concept_centroids_1536",
        "concept_centroids_1024_qwen",
        "concept_centroids_768",
        "concept_centroids_384",
    ];

    for tbl in tables {
        let mut sql = format!(
            "SELECT id, plugin_type, model_id, member_count, label FROM {tbl} WHERE is_active = TRUE"
        );
        if plugin_type.is_some() {
            sql.push_str(" AND plugin_type = $1");
        }
        if model_id.is_some() {
            if plugin_type.is_some() {
                sql.push_str(" AND model_id = $2");
            } else {
                sql.push_str(" AND model_id = $1");
            }
        }
        sql.push_str(" ORDER BY member_count DESC");

        let mut q = sqlx::query(&sql);
        if let Some(pt) = plugin_type {
            q = q.bind(pt);
        }
        if let Some(m) = model_id {
            q = q.bind(m);
        }

        if let Ok(rows) = q.fetch_all(db).await {
            for row in rows {
                let id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
                let p_type: String = row.try_get("plugin_type").map_err(|e| SmartFsError::Db(e.to_string()))?;
                let m_id: Uuid = row.try_get("model_id").map_err(|e| SmartFsError::Db(e.to_string()))?;
                let count: i64 = row.try_get("member_count").map_err(|e| SmartFsError::Db(e.to_string()))?;
                let label: Option<String> = row.try_get("label").ok();
                summaries.push(CentroidSummary {
                    id,
                    plugin_type: p_type,
                    model_id: m_id,
                    member_count: count,
                    label,
                });
            }
        }
    }

    summaries.sort_by_key(|s| std::cmp::Reverse(s.member_count));
    summaries.truncate(limit);
    Ok(summaries)
}
