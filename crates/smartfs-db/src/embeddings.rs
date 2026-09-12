use smartfs_schema::error::{Result, SmartFsError};
use sqlx::PgPool;
use uuid::Uuid;

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

/// @id: e8a12345-bcde-4f01-2345-6789abcdef01
/// Insert an embedding into `embeddings_1024_qwen` with denormalized `plugin_type` (ADR-49, ADR-50).
pub async fn insert_embedding_1024_qwen(
    pool: &PgPool,
    version_id: Uuid,
    model_id: Uuid,
    plugin_type: &str,
    embedding: &[f32],
) -> Result<()> {
    let vec_str = format_vector(embedding);
    sqlx::query(
        r#"
        INSERT INTO embeddings_1024_qwen (version_id, model_id, plugin_type, embedding)
        VALUES ($1, $2, $3, $4::vector)
        ON CONFLICT (version_id, model_id) DO NOTHING
        "#,
    )
    .bind(version_id)
    .bind(model_id)
    .bind(plugin_type)
    .bind(&vec_str)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("insert_embedding_1024_qwen error: {e}")))?;

    Ok(())
}

/// @id: f9b23456-cdef-5012-3456-789abcdef012
/// Insert an embedding into `embeddings_384` with denormalized `plugin_type`.
pub async fn insert_embedding_384(
    pool: &PgPool,
    version_id: Uuid,
    model_id: Uuid,
    plugin_type: &str,
    embedding: &[f32],
) -> Result<()> {
    let vec_str = format_vector(embedding);
    sqlx::query(
        r#"
        INSERT INTO embeddings_384 (version_id, model_id, plugin_type, embedding)
        VALUES ($1, $2, $3, $4::vector)
        ON CONFLICT (version_id, model_id) DO NOTHING
        "#,
    )
    .bind(version_id)
    .bind(model_id)
    .bind(plugin_type)
    .bind(&vec_str)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("insert_embedding_384 error: {e}")))?;

    Ok(())
}

/// @id: 0ac34567-def0-6123-4567-89abcdef0123
/// Insert an embedding into `embeddings_768` with denormalized `plugin_type`.
pub async fn insert_embedding_768(
    pool: &PgPool,
    version_id: Uuid,
    model_id: Uuid,
    plugin_type: &str,
    embedding: &[f32],
) -> Result<()> {
    let vec_str = format_vector(embedding);
    sqlx::query(
        r#"
        INSERT INTO embeddings_768 (version_id, model_id, plugin_type, embedding)
        VALUES ($1, $2, $3, $4::vector)
        ON CONFLICT (version_id, model_id) DO NOTHING
        "#,
    )
    .bind(version_id)
    .bind(model_id)
    .bind(plugin_type)
    .bind(&vec_str)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("insert_embedding_768 error: {e}")))?;

    Ok(())
}

/// @id: 1bd45678-ef01-7234-5678-9abcdef01234
/// Insert an embedding into `ast_embeddings_1536` with denormalized `plugin_type`.
pub async fn insert_ast_embedding_1536(
    pool: &PgPool,
    ast_node_id: Uuid,
    model_id: Uuid,
    plugin_type: &str,
    embedding: &[f32],
) -> Result<()> {
    let vec_str = format_vector(embedding);
    sqlx::query(
        r#"
        INSERT INTO ast_embeddings_1536 (ast_node_id, model_id, plugin_type, embedding)
        VALUES ($1, $2, $3, $4::vector)
        ON CONFLICT (ast_node_id, model_id) DO NOTHING
        "#,
    )
    .bind(ast_node_id)
    .bind(model_id)
    .bind(plugin_type)
    .bind(&vec_str)
    .execute(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("insert_ast_embedding_1536 error: {e}")))?;

    Ok(())
}

/// @id: 2ce56789-f012-8345-6789-abcdef012345
/// Look up the primary key UUID of the default embedding model.
pub async fn get_default_model_id(pool: &PgPool) -> Result<Uuid> {
    let model_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM embedding_models WHERE is_default = TRUE LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("get_default_model_id error: {e}")))?;

    Ok(model_id)
}

/// @id: 3df6789a-0123-9456-789a-bcdef0123456
/// Look up an embedding model by its exact name.
pub async fn get_model_id_by_name(pool: &PgPool, name: &str) -> Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT id FROM embedding_models WHERE name = $1 LIMIT 1")
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("get_model_id_by_name error: {e}")))
}
