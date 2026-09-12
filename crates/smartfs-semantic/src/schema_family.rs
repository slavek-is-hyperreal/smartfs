//! Schema mapping and vector utilities for database tables across vector dimensions.

use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// @id: 7a1b2c3d-4e5f-4012-89ab-cdef01234567
/// Supported vector storage families across the SmartFS database schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaFamily {
    Ast1536,
    General384,
    General768,
    General1024Qwen,
}

impl SchemaFamily {
    /// @id: 8b2c3d4e-5f60-4123-9abc-def012345678
    /// Resolves the schema family for a given model ID using the database.
    pub async fn from_model_id(db: &PgPool, model_id: Uuid) -> Result<Self> {
        let dim: i32 = sqlx::query_scalar("SELECT dimensions FROM embedding_models WHERE id = $1")
            .bind(model_id)
            .fetch_optional(db)
            .await
            .map_err(|e| SmartFsError::Db(format!("Failed to fetch embedding model: {e}")))?
            .ok_or_else(|| SmartFsError::NotFound(format!("Embedding model {model_id} not found")))?;

        match dim {
            1536 => Ok(SchemaFamily::Ast1536),
            1024 => Ok(SchemaFamily::General1024Qwen),
            768 => Ok(SchemaFamily::General768),
            384 => Ok(SchemaFamily::General384),
            other => Err(SmartFsError::Other(format!("Unsupported model dimension: {other}"))),
        }
    }

    /// @id: 9c3d4e5f-6012-4234-abcd-ef0123456789
    /// Resolves the schema family within an active SQL transaction.
    pub async fn from_model_id_tx(
        tx: &mut Transaction<'_, Postgres>,
        model_id: Uuid,
        vec_len_fallback: usize,
    ) -> Result<Self> {
        let dim: Option<i32> =
            sqlx::query_scalar("SELECT dimensions FROM embedding_models WHERE id = $1")
                .bind(model_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| SmartFsError::Db(format!("Failed to fetch model: {e}")))?;

        let d = dim.map(|d| d as usize).unwrap_or(vec_len_fallback);
        match d {
            1536 => Ok(SchemaFamily::Ast1536),
            1024 => Ok(SchemaFamily::General1024Qwen),
            768 => Ok(SchemaFamily::General768),
            384 => Ok(SchemaFamily::General384),
            other => Err(SmartFsError::Other(format!("Unsupported dimension: {other}"))),
        }
    }

    /// @id: 0d4e5f60-1234-4345-bcde-f01234567890
    /// Returns the vector dimension count for this schema family.
    pub fn dim(&self) -> usize {
        match self {
            SchemaFamily::Ast1536 => 1536,
            SchemaFamily::General1024Qwen => 1024,
            SchemaFamily::General768 => 768,
            SchemaFamily::General384 => 384,
        }
    }

    /// @id: 1e5f6012-2345-4456-cdef-012345678901
    /// Returns the table name for concept centroids in this family.
    pub fn centroid_table(&self) -> &'static str {
        match self {
            SchemaFamily::Ast1536 => "concept_centroids_1536",
            SchemaFamily::General1024Qwen => "concept_centroids_1024_qwen",
            SchemaFamily::General768 => "concept_centroids_768",
            SchemaFamily::General384 => "concept_centroids_384",
        }
    }

    /// @id: 2f601234-3456-4567-def0-123456789012
    /// Returns the table name for centroid members in this family.
    pub fn member_table(&self) -> &'static str {
        match self {
            SchemaFamily::Ast1536 => "centroid_members_1536",
            SchemaFamily::General1024Qwen => "centroid_members_1024_qwen",
            SchemaFamily::General768 => "centroid_members_768",
            SchemaFamily::General384 => "centroid_members_384",
        }
    }

    /// @id: 30123456-4567-4678-ef01-234567890123
    /// Returns the underlying source embeddings table name.
    pub fn embedding_table(&self) -> &'static str {
        match self {
            SchemaFamily::Ast1536 => "ast_embeddings_1536",
            SchemaFamily::General1024Qwen => "embeddings_1024_qwen",
            SchemaFamily::General768 => "embeddings_768",
            SchemaFamily::General384 => "embeddings_384",
        }
    }

    /// @id: 41234567-5678-4789-f012-345678901234
    /// Returns the primary member foreign key column name (`ast_node_id` or `version_id`).
    pub fn member_id_col(&self) -> &'static str {
        match self {
            SchemaFamily::Ast1536 => "ast_node_id",
            _ => "version_id",
        }
    }

    /// @id: 52345678-6789-4890-0123-456789012345
    /// Returns true if this family represents AST nodes.
    pub fn is_ast(&self) -> bool {
        matches!(self, SchemaFamily::Ast1536)
    }
}

/// @id: 63456789-7890-4901-1234-567890123456
/// Serializes a float slice into PostgreSQL pgvector text format `"[1.0,2.0,...]"`.
pub fn format_vector(vector: &[f32]) -> String {
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

/// @id: 74567890-8901-4a12-2345-678901234567
/// Parses a PostgreSQL pgvector text string `"[1.0,2.0,...]"` into a `Vec<f32>`.
pub fn parse_vector(s: &str) -> Result<Vec<f32>> {
    let s = s.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|item| {
            item.trim().parse::<f32>().map_err(|e| {
                SmartFsError::Db(format!("Failed to parse vector component '{item}': {e}"))
            })
        })
        .collect()
}
