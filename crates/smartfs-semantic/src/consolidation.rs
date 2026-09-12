//! Core batch consolidation, centroid attachment, splitting, and merging.

use crate::config::ConsolidationConfig;
use crate::kmeans::{combine_centroids, kmeans2, variance_from_m2, welford_update};
use crate::schema_family::{format_vector, parse_vector, SchemaFamily};
use crate::types::{BufferedVector, CentroidMemberWithVector, ConceptCentroid};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

/// @id: 1f4d92a8-0e31-4c75-812d-9b6f3a0e1c22
/// Counts unconsolidated vectors for a specific `(plugin_type, model_id)` combination.
pub async fn count_unconsolidated(db: &PgPool, plugin_type: &str, model_id: Uuid) -> Result<i64> {
    let family = SchemaFamily::from_model_id(db, model_id).await?;
    let sql = if family.is_ast() {
        "SELECT COUNT(*) FROM ast_embeddings_1536 WHERE consolidated = FALSE AND is_current = TRUE AND plugin_type = $1 AND model_id = $2"
    } else {
        match family {
            SchemaFamily::General1024Qwen => {
                "SELECT COUNT(*) FROM embeddings_1024_qwen WHERE consolidated = FALSE AND plugin_type = $1 AND model_id = $2"
            }
            SchemaFamily::General768 => {
                "SELECT COUNT(*) FROM embeddings_768 WHERE consolidated = FALSE AND plugin_type = $1 AND model_id = $2"
            }
            SchemaFamily::General384 => {
                "SELECT COUNT(*) FROM embeddings_384 WHERE consolidated = FALSE AND plugin_type = $1 AND model_id = $2"
            }
            _ => unreachable!(),
        }
    };

    let count: i64 = sqlx::query_scalar(sql)
        .bind(plugin_type)
        .bind(model_id)
        .fetch_one(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("count_unconsolidated error: {e}")))?;

    Ok(count)
}

/// @id: c4e19a7d-5b2f-4e88-a1c3-9f6d0b8e2a55
/// Claims a batch of unconsolidated vectors using `FOR UPDATE SKIP LOCKED`.
pub async fn claim_unconsolidated_batch(
    tx: &mut Transaction<'_, Postgres>,
    plugin_type: &str,
    model_id: Uuid,
    limit: i64,
) -> Result<Vec<BufferedVector>> {
    let family = SchemaFamily::from_model_id_tx(tx, model_id, 0).await?;
    let sql = if family.is_ast() {
        r#"
        SELECT ast_node_id AS id, embedding::text AS vec_text, plugin_type, model_id
        FROM ast_embeddings_1536
        WHERE consolidated = FALSE AND is_current = TRUE
          AND plugin_type = $1 AND model_id = $2
        ORDER BY created_at ASC
        FOR UPDATE SKIP LOCKED
        LIMIT $3
        "#
        .to_string()
    } else {
        format!(
            r#"
            SELECT version_id AS id, embedding::text AS vec_text, plugin_type, model_id
            FROM {}
            WHERE consolidated = FALSE
              AND plugin_type = $1 AND model_id = $2
            ORDER BY created_at ASC
            FOR UPDATE SKIP LOCKED
            LIMIT $3
            "#,
            family.embedding_table()
        )
    };

    let rows = sqlx::query(&sql)
        .bind(plugin_type)
        .bind(model_id)
        .bind(limit)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("claim_unconsolidated_batch error: {e}")))?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let vec_text: String = row.try_get("vec_text").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let p_type: String = row.try_get("plugin_type").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let m_id: Uuid = row.try_get("model_id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let embedding = parse_vector(&vec_text)?;

        result.push(BufferedVector {
            ast_node_id: if family.is_ast() { Some(id) } else { None },
            version_id: if family.is_ast() { None } else { Some(id) },
            plugin_type: p_type,
            model_id: m_id,
            embedding,
        });
    }

    Ok(result)
}

/// @id: 4e9a1c87-3b2d-4f11-9a72-6d5f0b8e1a33
/// Finds the nearest active centroid for an item within the current transaction.
pub async fn nearest_centroid(
    tx: &mut Transaction<'_, Postgres>,
    plugin_type: &str,
    model_id: Uuid,
    item: &BufferedVector,
) -> Result<Option<(ConceptCentroid, f64)>> {
    let family = SchemaFamily::from_model_id_tx(tx, model_id, item.embedding.len()).await?;
    let vec_str = format_vector(&item.embedding);
    let sql = format!(
        r#"
        SELECT
            id, plugin_type, model_id, centroid::text AS centroid_text,
            m2, member_count, label, is_active, merged_into,
            created_at, last_consolidated_at,
            (centroid <=> $3::vector)::float8 AS distance
        FROM {}
        WHERE is_active = TRUE
          AND plugin_type = $1
          AND model_id = $2
        ORDER BY centroid <=> $3::vector ASC
        LIMIT 1
        "#,
        family.centroid_table()
    );

    let row = sqlx::query(&sql)
        .bind(plugin_type)
        .bind(model_id)
        .bind(&vec_str)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("nearest_centroid error: {e}")))?;

    match row {
        Some(r) => {
            let id: Uuid = r.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let p_type: String = r.try_get("plugin_type").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let m_id: Uuid = r.try_get("model_id").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let c_text: String = r.try_get("centroid_text").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let m2: f64 = r.try_get("m2").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let member_count: i64 = r.try_get("member_count").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let label: Option<String> = r.try_get("label").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let is_active: bool = r.try_get("is_active").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let merged_into: Option<Uuid> = r.try_get("merged_into").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let created_at = r.try_get("created_at").ok();
            let last_consolidated_at = r.try_get("last_consolidated_at").ok();
            let distance: f64 = r.try_get("distance").map_err(|e| SmartFsError::Db(e.to_string()))?;

            let centroid = ConceptCentroid {
                id,
                plugin_type: p_type,
                model_id: m_id,
                centroid: parse_vector(&c_text)?,
                m2,
                member_count,
                label,
                is_active,
                merged_into,
                created_at,
                last_consolidated_at,
            };
            Ok(Some((centroid, distance)))
        }
        None => Ok(None),
    }
}

/// @id: 2d8b4e19-6c3a-4f90-8e12-5a7c9f0b2d44
/// Creates a new concept centroid seeded from an unconsolidated buffered vector.
pub async fn create_centroid_from(
    tx: &mut Transaction<'_, Postgres>,
    plugin_type: &str,
    model_id: Uuid,
    item: &BufferedVector,
) -> Result<ConceptCentroid> {
    let family = SchemaFamily::from_model_id_tx(tx, model_id, item.embedding.len()).await?;
    let new_id = Uuid::new_v4();
    let vec_str = format_vector(&item.embedding);

    let insert_sql = format!(
        r#"
        INSERT INTO {} (
            id, plugin_type, model_id, centroid, m2, member_count, label, is_active, created_at, last_consolidated_at
        ) VALUES (
            $1, $2, $3, $4::vector, 0.0, 1, NULL, TRUE, NOW(), NOW()
        )
        "#,
        family.centroid_table()
    );

    sqlx::query(&insert_sql)
        .bind(new_id)
        .bind(plugin_type)
        .bind(model_id)
        .bind(&vec_str)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("create_centroid_from insert error: {e}")))?;

    if family.is_ast() {
        let ast_id = item.ast_node_id.ok_or_else(|| {
            SmartFsError::Other("Missing ast_node_id for AST centroid member".to_string())
        })?;
        sqlx::query(
            r#"
            INSERT INTO centroid_members_1536 (centroid_id, ast_node_id, distance, consolidated_at)
            VALUES ($1, $2, 0.0, NOW())
            ON CONFLICT (centroid_id, ast_node_id) DO UPDATE SET distance = 0.0, consolidated_at = NOW()
            "#,
        )
        .bind(new_id)
        .bind(ast_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("insert centroid member error: {e}")))?;
    } else {
        let ver_id = item.version_id.ok_or_else(|| {
            SmartFsError::Other("Missing version_id for file centroid member".to_string())
        })?;
        let member_sql = format!(
            r#"
            INSERT INTO {} (centroid_id, version_id, distance, consolidated_at)
            VALUES ($1, $2, 0.0, NOW())
            ON CONFLICT (centroid_id, version_id) DO UPDATE SET distance = 0.0, consolidated_at = NOW()
            "#,
            family.member_table()
        );
        sqlx::query(&member_sql)
            .bind(new_id)
            .bind(ver_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(format!("insert centroid member error: {e}")))?;
    }

    Ok(ConceptCentroid {
        id: new_id,
        plugin_type: plugin_type.to_string(),
        model_id,
        centroid: item.embedding.clone(),
        m2: 0.0,
        member_count: 1,
        label: None,
        is_active: true,
        merged_into: None,
        created_at: Some(chrono::Utc::now()),
        last_consolidated_at: Some(chrono::Utc::now()),
    })
}

/// @id: a3f8d1c6-7e29-4a55-b0d4-9c2e5f8a1b77
/// Attaches a buffered vector to an existing centroid, performing a Welford update
/// and triggering a split if variance or member limits are exceeded.
pub async fn attach_to_centroid(
    tx: &mut Transaction<'_, Postgres>,
    centroid: &ConceptCentroid,
    item: &BufferedVector,
    dist: f64,
    cfg: &ConsolidationConfig,
) -> Result<()> {
    let family = SchemaFamily::from_model_id_tx(tx, centroid.model_id, item.embedding.len()).await?;
    let mut mean = centroid.centroid.clone();
    let mut m2 = centroid.m2;
    let mut count = centroid.member_count;

    welford_update(&mut mean, &mut m2, &mut count, &item.embedding);
    let variance = variance_from_m2(m2, count);

    let vec_str = format_vector(&mean);
    let update_sql = format!(
        r#"
        UPDATE {}
        SET centroid = $2::vector, m2 = $3, member_count = $4, last_consolidated_at = NOW()
        WHERE id = $1
        "#,
        family.centroid_table()
    );

    sqlx::query(&update_sql)
        .bind(centroid.id)
        .bind(&vec_str)
        .bind(m2)
        .bind(count)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("update centroid error: {e}")))?;

    if family.is_ast() {
        let ast_id = item.ast_node_id.ok_or_else(|| {
            SmartFsError::Other("Missing ast_node_id for AST member".to_string())
        })?;
        sqlx::query(
            r#"
            INSERT INTO centroid_members_1536 (centroid_id, ast_node_id, distance, consolidated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (centroid_id, ast_node_id) DO UPDATE SET distance = EXCLUDED.distance, consolidated_at = NOW()
            "#,
        )
        .bind(centroid.id)
        .bind(ast_id)
        .bind(dist)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("insert centroid member error: {e}")))?;
    } else {
        let ver_id = item.version_id.ok_or_else(|| {
            SmartFsError::Other("Missing version_id for file member".to_string())
        })?;
        let member_sql = format!(
            r#"
            INSERT INTO {} (centroid_id, version_id, distance, consolidated_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (centroid_id, version_id) DO UPDATE SET distance = EXCLUDED.distance, consolidated_at = NOW()
            "#,
            family.member_table()
        );
        sqlx::query(&member_sql)
            .bind(centroid.id)
            .bind(ver_id)
            .bind(dist)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(format!("insert centroid member error: {e}")))?;
    }

    if variance > cfg.split_variance_threshold || count > cfg.max_members_per_centroid {
        split_centroid(tx, centroid.id, cfg).await?;
    }

    Ok(())
}

/// @id: 3f9a1b2c-4d5e-6f70-8a9b-0c1d2e3f4a5b
/// Marks a buffered vector as consolidated in its source embedding table.
pub async fn mark_consolidated(
    tx: &mut Transaction<'_, Postgres>,
    item: &BufferedVector,
) -> Result<()> {
    if let Some(ast_id) = item.ast_node_id {
        sqlx::query(
            "UPDATE ast_embeddings_1536 SET consolidated = TRUE WHERE ast_node_id = $1 AND model_id = $2",
        )
        .bind(ast_id)
        .bind(item.model_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(e.to_string()))?;
    } else if let Some(ver_id) = item.version_id {
        let family = SchemaFamily::from_model_id_tx(tx, item.model_id, item.embedding.len()).await?;
        let sql = format!(
            "UPDATE {} SET consolidated = TRUE WHERE version_id = $1 AND model_id = $2",
            family.embedding_table()
        );
        sqlx::query(&sql)
            .bind(ver_id)
            .bind(item.model_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
    }
    Ok(())
}

/// @id: 5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a16
/// Splits a centroid using local k-means (k=2).
///
/// Invariant:
/// - Deactivates the parent centroid (`is_active = FALSE`), never deleting rows (ADR-53).
pub async fn split_centroid(
    tx: &mut Transaction<'_, Postgres>,
    centroid_id: Uuid,
    _cfg: &ConsolidationConfig,
) -> Result<()> {
    let mut found_family: Option<(SchemaFamily, String, Uuid)> = None;

    for family in [
        SchemaFamily::Ast1536,
        SchemaFamily::General1024Qwen,
        SchemaFamily::General768,
        SchemaFamily::General384,
    ] {
        let sql = format!(
            "SELECT plugin_type, model_id FROM {} WHERE id = $1 AND is_active = TRUE",
            family.centroid_table()
        );
        if let Some(row) = sqlx::query(&sql)
            .bind(centroid_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?
        {
            let plugin_type: String = row.try_get("plugin_type").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let model_id: Uuid = row.try_get("model_id").map_err(|e| SmartFsError::Db(e.to_string()))?;
            found_family = Some((family, plugin_type, model_id));
            break;
        }
    }

    let (family, plugin_type, model_id) = match found_family {
        Some(f) => f,
        None => return Ok(()),
    };

    let fetch_sql = if family.is_ast() {
        r#"
        SELECT cm.ast_node_id AS id, ae.embedding::text AS vec_text
        FROM centroid_members_1536 cm
        JOIN ast_embeddings_1536 ae ON cm.ast_node_id = ae.ast_node_id
        WHERE cm.centroid_id = $1
        "#
        .to_string()
    } else {
        format!(
            r#"
            SELECT cm.version_id AS id, e.embedding::text AS vec_text
            FROM {} cm
            JOIN {} e ON cm.version_id = e.version_id
            WHERE cm.centroid_id = $1
            "#,
            family.member_table(),
            family.embedding_table()
        )
    };

    let member_rows = sqlx::query(&fetch_sql)
        .bind(centroid_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("fetch centroid members error: {e}")))?;

    if member_rows.len() < 2 {
        return Ok(());
    }

    let mut members_with_vec = Vec::with_capacity(member_rows.len());
    for row in member_rows {
        let id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let vec_text: String = row.try_get("vec_text").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let vector = parse_vector(&vec_text)?;
        members_with_vec.push(CentroidMemberWithVector { id, vector });
    }

    let (cluster_a, cluster_b) = kmeans2(&members_with_vec)?;

    let new_a_id = Uuid::new_v4();
    let new_b_id = Uuid::new_v4();

    let insert_centroid_sql = format!(
        r#"
        INSERT INTO {} (
            id, plugin_type, model_id, centroid, m2, member_count, label, is_active, created_at, last_consolidated_at
        ) VALUES (
            $1, $2, $3, $4::vector, $5, $6, NULL, TRUE, NOW(), NOW()
        )
        "#,
        family.centroid_table()
    );

    let vec_a_str = format_vector(&cluster_a.mean);
    sqlx::query(&insert_centroid_sql)
        .bind(new_a_id)
        .bind(&plugin_type)
        .bind(model_id)
        .bind(&vec_a_str)
        .bind(cluster_a.m2)
        .bind(cluster_a.count)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("create centroid a error: {e}")))?;

    let vec_b_str = format_vector(&cluster_b.mean);
    sqlx::query(&insert_centroid_sql)
        .bind(new_b_id)
        .bind(&plugin_type)
        .bind(model_id)
        .bind(&vec_b_str)
        .bind(cluster_b.m2)
        .bind(cluster_b.count)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("create centroid b error: {e}")))?;

    let member_insert_sql = format!(
        r#"
        INSERT INTO {} (centroid_id, {}, distance, consolidated_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (centroid_id, {}) DO UPDATE SET distance = EXCLUDED.distance, consolidated_at = NOW()
        "#,
        family.member_table(),
        family.member_id_col(),
        family.member_id_col()
    );

    for (id, dist) in cluster_a.member_ids.iter().zip(&cluster_a.member_distances) {
        sqlx::query(&member_insert_sql)
            .bind(new_a_id)
            .bind(id)
            .bind(dist)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(format!("insert member for cluster a: {e}")))?;
    }

    for (id, dist) in cluster_b.member_ids.iter().zip(&cluster_b.member_distances) {
        sqlx::query(&member_insert_sql)
            .bind(new_b_id)
            .bind(id)
            .bind(dist)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(format!("insert member for cluster b: {e}")))?;
    }

    let del_members_sql = format!("DELETE FROM {} WHERE centroid_id = $1", family.member_table());
    sqlx::query(&del_members_sql)
        .bind(centroid_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(e.to_string()))?;

    // Invariant: Never DELETE centroids, only deactivate
    let deactivate_sql = format!(
        "UPDATE {} SET is_active = FALSE WHERE id = $1",
        family.centroid_table()
    );
    sqlx::query(&deactivate_sql)
        .bind(centroid_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(e.to_string()))?;

    Ok(())
}

/// @id: 7a2c9e4f-3b86-4d17-9f0a-5e8c2d6b3a71
/// Merges pairs of active centroids closer than `merge_threshold` using Chan et al.'s parallel variance formula.
///
/// Invariant:
/// - Deactivates source centroids (`is_active = FALSE`) with `merged_into` pointing to the new centroid (ADR-53).
pub async fn merge_centroids(
    tx: &mut Transaction<'_, Postgres>,
    plugin_type: &str,
    model_id: Uuid,
    merge_threshold: f64,
) -> Result<usize> {
    let family = SchemaFamily::from_model_id_tx(tx, model_id, 0).await?;

    let sql = format!(
        r#"
        SELECT
            a.id AS a_id, a.centroid::text AS a_centroid, a.m2 AS a_m2, a.member_count AS a_count,
            b.id AS b_id, b.centroid::text AS b_centroid, b.m2 AS b_m2, b.member_count AS b_count
        FROM {} a
        JOIN {} b ON a.id < b.id
                 AND a.plugin_type = b.plugin_type
                 AND a.model_id = b.model_id
        WHERE a.plugin_type = $1
          AND a.model_id = $2
          AND a.is_active = TRUE
          AND b.is_active = TRUE
          AND (a.centroid <=> b.centroid) < $3
        ORDER BY (a.centroid <=> b.centroid) ASC
        LIMIT 50
        "#,
        family.centroid_table(),
        family.centroid_table()
    );

    let rows = sqlx::query(&sql)
        .bind(plugin_type)
        .bind(model_id)
        .bind(merge_threshold)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| SmartFsError::Db(format!("find_mergeable_pairs error: {e}")))?;

    let mut merged_ids: HashSet<Uuid> = HashSet::new();
    let mut n = 0usize;

    for row in rows {
        let a_id: Uuid = row.try_get("a_id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let b_id: Uuid = row.try_get("b_id").map_err(|e| SmartFsError::Db(e.to_string()))?;

        if merged_ids.contains(&a_id) || merged_ids.contains(&b_id) {
            continue;
        }

        let a_centroid: String = row.try_get("a_centroid").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let a_m2: f64 = row.try_get("a_m2").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let a_count: i64 = row.try_get("a_count").map_err(|e| SmartFsError::Db(e.to_string()))?;

        let b_centroid: String = row.try_get("b_centroid").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let b_m2: f64 = row.try_get("b_m2").map_err(|e| SmartFsError::Db(e.to_string()))?;
        let b_count: i64 = row.try_get("b_count").map_err(|e| SmartFsError::Db(e.to_string()))?;

        let centroid_a = ConceptCentroid {
            id: a_id,
            plugin_type: plugin_type.to_string(),
            model_id,
            centroid: parse_vector(&a_centroid)?,
            m2: a_m2,
            member_count: a_count,
            label: None,
            is_active: true,
            merged_into: None,
            created_at: None,
            last_consolidated_at: None,
        };

        let centroid_b = ConceptCentroid {
            id: b_id,
            plugin_type: plugin_type.to_string(),
            model_id,
            centroid: parse_vector(&b_centroid)?,
            m2: b_m2,
            member_count: b_count,
            label: None,
            is_active: true,
            merged_into: None,
            created_at: None,
            last_consolidated_at: None,
        };

        let merged = combine_centroids(&centroid_a, &centroid_b);
        let new_id = Uuid::new_v4();
        let vec_str = format_vector(&merged.mean);

        let insert_sql = format!(
            r#"
            INSERT INTO {} (
                id, plugin_type, model_id, centroid, m2, member_count, label, is_active, created_at, last_consolidated_at
            ) VALUES (
                $1, $2, $3, $4::vector, $5, $6, NULL, TRUE, NOW(), NOW()
            )
            "#,
            family.centroid_table()
        );

        sqlx::query(&insert_sql)
            .bind(new_id)
            .bind(plugin_type)
            .bind(model_id)
            .bind(&vec_str)
            .bind(merged.m2)
            .bind(merged.count)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(format!("create merged centroid error: {e}")))?;

        let reparent_sql_1 = format!(
            "UPDATE {} SET centroid_id = $1 WHERE centroid_id = $2",
            family.member_table()
        );
        sqlx::query(&reparent_sql_1)
            .bind(new_id)
            .bind(a_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        let reparent_sql_2 = format!(
            r#"
            INSERT INTO {} (centroid_id, {}, distance, consolidated_at)
            SELECT $1, {}, distance, consolidated_at
            FROM {}
            WHERE centroid_id = $2
            ON CONFLICT (centroid_id, {}) DO NOTHING
            "#,
            family.member_table(),
            family.member_id_col(),
            family.member_id_col(),
            family.member_table(),
            family.member_id_col()
        );
        sqlx::query(&reparent_sql_2)
            .bind(new_id)
            .bind(b_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        let del_b_members = format!("DELETE FROM {} WHERE centroid_id = $1", family.member_table());
        sqlx::query(&del_b_members)
            .bind(b_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        let deactivate_sql = format!(
            "UPDATE {} SET is_active = FALSE, merged_into = $2 WHERE id = $1",
            family.centroid_table()
        );
        sqlx::query(&deactivate_sql)
            .bind(a_id)
            .bind(new_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        sqlx::query(&deactivate_sql)
            .bind(b_id)
            .bind(new_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        merged_ids.insert(a_id);
        merged_ids.insert(b_id);
        n += 1;
    }

    Ok(n)
}

/// @id: 8d3a6f01-2c59-4d77-b6e4-1f8a3c5d9e02
/// Consolidates a batch of working memory vectors for a single `(plugin_type, model_id)`.
///
/// Steps:
/// 1. Claims unconsolidated rows with `FOR UPDATE SKIP LOCKED`.
/// 2. Finds nearest centroid (within `join_threshold`) or creates a new one.
/// 3. Marks processed rows as `consolidated = TRUE`.
/// 4. Commits in a single short transaction.
pub async fn consolidate_batch(
    db: &PgPool,
    plugin_type: &str,
    model_id: Uuid,
    cfg: &ConsolidationConfig,
) -> Result<usize> {
    let mut tx = db.begin().await.map_err(|e| SmartFsError::Db(e.to_string()))?;
    let batch = claim_unconsolidated_batch(&mut tx, plugin_type, model_id, cfg.batch_size).await?;
    let mut n = 0;

    for item in &batch {
        match nearest_centroid(&mut tx, plugin_type, model_id, item).await? {
            Some((centroid, dist)) if dist <= cfg.join_threshold => {
                attach_to_centroid(&mut tx, &centroid, item, dist, cfg).await?;
            }
            _ => {
                create_centroid_from(&mut tx, plugin_type, model_id, item).await?;
            }
        }
        mark_consolidated(&mut tx, item).await?;
        n += 1;
    }

    tx.commit().await.map_err(|e| SmartFsError::Db(e.to_string()))?;
    Ok(n)
}
