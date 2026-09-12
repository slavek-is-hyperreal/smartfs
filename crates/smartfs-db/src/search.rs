use crate::models::{FulltextHit, FulltextHitKind};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// @id: 3d8e1f6a-7c42-4b90-a5e8-6c1f9d3b7a02
/// Return sum of rows where `consolidated = FALSE` across ALL embedding tables at once
/// (embeddings_384, embeddings_768, embeddings_1024_qwen, ast_embeddings_1536 WHERE is_current).
/// Used by `smartfs-semantic::count_unconsolidated` as a single query (ADR-50, ADR-53).
pub async fn count_all_unconsolidated(pool: &PgPool) -> Result<i64> {
    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT
            (SELECT COUNT(*) FROM embeddings_384 WHERE consolidated = FALSE)
          + (SELECT COUNT(*) FROM embeddings_768 WHERE consolidated = FALSE)
          + (SELECT COUNT(*) FROM embeddings_1024_qwen WHERE consolidated = FALSE)
          + (SELECT COUNT(*) FROM ast_embeddings_1536 WHERE consolidated = FALSE AND is_current = TRUE)
          AS total_unconsolidated
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("count_all_unconsolidated error: {e}")))?;

    Ok(total)
}

/// @id: 8c1e3f7a-2468-4ace-9bdf-02468ace1357
/// Set or clear `file_versions.search_text` (ADR-54).
/// This is the only legal way to write this column, invoked by `smartfs-ai` during generic embedding pass.
pub async fn set_search_text(
    pool: &PgPool,
    version_id: Uuid,
    text: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE file_versions SET search_text = $1 WHERE id = $2")
        .bind(text)
        .bind(version_id)
        .execute(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("set_search_text error: {e}")))?;

    Ok(())
}

/// @id: a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d
/// Fulltext search querying both file level (`file_versions.search_text`) and
/// code AST level (`ast_nodes.source`).
///
/// Uses `pg_search` BM25 if available (ADR-54), with transparent fallback to
/// substring search if `pg_search` extension is not active in the database instance.
pub async fn search_fulltext_bm25(
    pool: &PgPool,
    query: &str,
    plugin_type: Option<&str>,
    limit: i64,
) -> Result<Vec<FulltextHit>> {
    // Check if pg_search extension is available
    let has_pg_search: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_search')",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if has_pg_search {
        if let Ok(hits) = run_bm25_search(pool, query, plugin_type, limit).await {
            return Ok(hits);
        }
    }

    run_fallback_search(pool, query, plugin_type, limit).await
}

async fn run_bm25_search(
    pool: &PgPool,
    query: &str,
    plugin_type: Option<&str>,
    limit: i64,
) -> std::result::Result<Vec<FulltextHit>, sqlx::Error> {
    // ParadeDB pg_search uses @@@ operator and paradedb.score()
    let sql = r#"
        WITH file_hits AS (
            SELECT
                fv.id AS id,
                'file' AS kind_str,
                fv.id AS version_id,
                ir.name AS name,
                SUBSTRING(COALESCE(fv.search_text, '') FROM 1 FOR 300) AS snippet,
                paradedb.score(fv.id)::real AS score,
                fv.special_type AS plugin_type
            FROM file_versions fv
            JOIN inode_registry ir ON fv.inode_id = ir.id
            WHERE fv.search_text @@@ $1
              AND ($2::text IS NULL OR fv.special_type = $2)
            LIMIT $3
        ),
        ast_hits AS (
            SELECT
                an.id AS id,
                'ast_node' AS kind_str,
                an.version_id AS version_id,
                an.name AS name,
                SUBSTRING(an.source FROM 1 FOR 300) AS snippet,
                paradedb.score(an.id)::real AS score,
                fv.special_type AS plugin_type
            FROM ast_nodes an
            JOIN file_versions fv ON an.version_id = fv.id
            WHERE an.source @@@ $1
              AND ($2::text IS NULL OR fv.special_type = $2)
            LIMIT $3
        )
        SELECT * FROM file_hits
        UNION ALL
        SELECT * FROM ast_hits
        ORDER BY score DESC
        LIMIT $3
    "#;

    let rows = sqlx::query(sql)
        .bind(query)
        .bind(plugin_type)
        .bind(limit)
        .fetch_all(pool)
        .await?;

    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let kind_str: String = row.try_get("kind_str")?;
        let version_id: Uuid = row.try_get("version_id")?;
        let name: Option<String> = row.try_get("name")?;
        let snippet: String = row.try_get("snippet")?;
        let score: f32 = row.try_get("score")?;
        let p_type: String = row.try_get("plugin_type")?;

        let kind = if kind_str == "ast_node" {
            FulltextHitKind::AstNode
        } else {
            FulltextHitKind::File
        };

        results.push(FulltextHit {
            id,
            kind,
            version_id,
            name,
            snippet,
            score,
            plugin_type: p_type,
        });
    }

    Ok(results)
}

async fn run_fallback_search(
    pool: &PgPool,
    query: &str,
    plugin_type: Option<&str>,
    limit: i64,
) -> Result<Vec<FulltextHit>> {
    let pattern = format!("%{query}%");
    let sql = r#"
        WITH file_hits AS (
            SELECT
                fv.id AS id,
                'file' AS kind_str,
                fv.id AS version_id,
                ir.name AS name,
                SUBSTRING(COALESCE(fv.search_text, '') FROM 1 FOR 300) AS snippet,
                1.0::real AS score,
                fv.special_type AS plugin_type
            FROM file_versions fv
            JOIN inode_registry ir ON fv.inode_id = ir.id
            WHERE fv.search_text IS NOT NULL
              AND fv.search_text ILIKE $1
              AND ($2::text IS NULL OR fv.special_type = $2)
            LIMIT $3
        ),
        ast_hits AS (
            SELECT
                an.id AS id,
                'ast_node' AS kind_str,
                an.version_id AS version_id,
                an.name AS name,
                SUBSTRING(an.source FROM 1 FOR 300) AS snippet,
                1.0::real AS score,
                fv.special_type AS plugin_type
            FROM ast_nodes an
            JOIN file_versions fv ON an.version_id = fv.id
            WHERE (an.source ILIKE $1 OR an.name ILIKE $1)
              AND ($2::text IS NULL OR fv.special_type = $2)
            LIMIT $3
        )
        SELECT * FROM file_hits
        UNION ALL
        SELECT * FROM ast_hits
        ORDER BY score DESC
        LIMIT $3
    "#;

    let rows = sqlx::query(sql)
        .bind(&pattern)
        .bind(plugin_type)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("search_fulltext fallback error: {e}")))?;

    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let kind_str: String = row
            .try_get("kind_str")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let version_id: Uuid = row
            .try_get("version_id")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let name: Option<String> = row
            .try_get("name")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let snippet: String = row
            .try_get("snippet")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let score: f32 = row
            .try_get("score")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;
        let p_type: String = row
            .try_get("plugin_type")
            .map_err(|e| SmartFsError::Db(e.to_string()))?;

        let kind = if kind_str == "ast_node" {
            FulltextHitKind::AstNode
        } else {
            FulltextHitKind::File
        };

        results.push(FulltextHit {
            id,
            kind,
            version_id,
            name,
            snippet,
            score,
            plugin_type: p_type,
        });
    }

    Ok(results)
}
