//! smartfs-semantic — Centroid graph, working memory buffer, consolidation, and lexical graph.
//!
//! Exclusively owns:
//! - Centroid graphs (`concept_centroids_*`, `centroid_members_*`)
//! - Lexical graph (`lexical_nodes`, `lexical_edges`, `word_centroid_links_*`)
//! - `consolidation_thresholds` table
//! - Background consolidation and merge supervisor per `(plugin_type, model_id)`
//!
//! Strictly enforces:
//! - NEVER import `smartfs-ai`
//! - NEVER use DELETE on `concept_centroids_*` — only `is_active = FALSE` and `merged_into` (ADR-53)
//! - Local batch processing only — never recalculate centroids globally
//! - Always hold `pg_try_advisory_lock` during batch consolidation and merge

pub mod config;
pub mod consolidation;
pub mod kmeans;
pub mod lexical;
pub mod schema_family;
pub mod search;
pub mod supervisor;
pub mod types;

pub use config::{calibrate_join_threshold, load_consolidation_config, ConsolidationConfig};
pub use consolidation::{
    attach_to_centroid, claim_unconsolidated_batch, consolidate_batch, count_unconsolidated,
    create_centroid_from, mark_consolidated, merge_centroids, nearest_centroid, split_centroid,
};
pub use kmeans::{
    combine_centroids, cosine_distance, kmeans2, variance_from_m2, welford_update, Cluster,
};
pub use lexical::{
    import_wordnet, label_centroid_from_members, link_word_to_centroid, tokenize_identifier,
    LexicalNode, LexicalRelation, WordCentroidLink, WordnetEdgeImport, WordnetImport,
    WordnetNodeImport,
};
pub use search::{
    brute_force_unconsolidated, list_active_centroids, merge_by_similarity, search_by_concept,
    search_via_centroids, ConceptSearchHit, HitSource,
};
pub use supervisor::{
    advisory_lock_key, consolidation_supervisor, fetch_calibrated_combinations, pg_hash_bytes,
    release_advisory_lock, spawn_all_consolidation_supervisors, try_advisory_lock,
};
pub use types::{
    BufferedVector, CentroidMember, CentroidMemberWithVector, CentroidSummary, ConceptCentroid,
};
