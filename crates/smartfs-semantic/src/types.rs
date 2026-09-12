//! Types for concept centroids, members, and buffered vectors.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// @id: 97f394c2-959c-482a-a9a3-5c2f8216c5b1
/// Represents a crystallized concept centroid in the database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConceptCentroid {
    pub id: Uuid,
    pub plugin_type: String,
    pub model_id: Uuid,
    pub centroid: Vec<f32>,
    pub m2: f64,
    pub member_count: i64,
    pub label: Option<String>,
    pub is_active: bool,
    pub merged_into: Option<Uuid>,
    pub created_at: Option<DateTime<Utc>>,
    pub last_consolidated_at: Option<DateTime<Utc>>,
}

/// @id: c088d8b6-932f-48e0-bb15-0d05da9319bf
/// Represents a membership record linking an AST node or file version to a centroid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CentroidMember {
    pub centroid_id: Uuid,
    pub ast_node_id: Option<Uuid>,
    pub version_id: Option<Uuid>,
    pub distance: f64,
    pub consolidated_at: DateTime<Utc>,
}

/// @id: 61ef53fa-1b42-49da-9a25-a1312ecf01f8
/// Represents an unconsolidated vector claimed from the working memory buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BufferedVector {
    pub ast_node_id: Option<Uuid>,
    pub version_id: Option<Uuid>,
    pub plugin_type: String,
    pub model_id: Uuid,
    pub embedding: Vec<f32>,
}

/// @id: bfe15b9c-7201-4475-b6d3-e7a8e747b0e1
/// Centroid member along with its underlying embedding vector, used during splitting.
#[derive(Debug, Clone, PartialEq)]
pub struct CentroidMemberWithVector {
    pub id: Uuid,
    pub vector: Vec<f32>,
}
