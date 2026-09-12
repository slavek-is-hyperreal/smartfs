//! Handler for `concepts` command.

use smartfs_db::PgPool;
use smartfs_schema::error::Result;
use uuid::Uuid;

use crate::args::ConceptsArgs;

/// @id: e6a1b2c3-3007-4000-8000-000000000001
/// Summary of a calibrated concept cluster / centroid configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConceptSummary {
    pub plugin_type: String,
    pub model_id: Uuid,
    pub join_threshold: f64,
    pub backlog_threshold: i64,
    pub unconsolidated_count: i64,
}

/// @id: e6a1b2c3-3007-4000-8000-000000000002
/// Result of the `concepts` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ConceptsResult {
    pub concepts: Vec<ConceptSummary>,
}

/// @id: e6a1b2c3-3007-4000-8000-000000000003
/// Handles execution of the `concepts` command.
pub async fn handle_concepts(
    pool: &PgPool,
    args: &ConceptsArgs,
) -> Result<ConceptsResult> {
    let combos = smartfs_semantic::fetch_calibrated_combinations(pool)
        .await
        .unwrap_or_default();

    let mut concepts = Vec::new();

    for (p_type, m_id) in combos {
        if let Some(target) = &args.plugin_type {
            if &p_type != target {
                continue;
            }
        }

        let cfg = smartfs_semantic::load_consolidation_config(pool, &p_type, m_id).await?;
        let unconsolidated = smartfs_semantic::count_unconsolidated(pool, &p_type, m_id)
            .await
            .unwrap_or(0);

        concepts.push(ConceptSummary {
            plugin_type: p_type,
            model_id: m_id,
            join_threshold: cfg.join_threshold,
            backlog_threshold: cfg.backlog_threshold,
            unconsolidated_count: unconsolidated,
        });
    }

    // Sort by unconsolidated count descending
    concepts.sort_by_key(|c| std::cmp::Reverse(c.unconsolidated_count));

    if let Some(limit) = args.limit {
        concepts.truncate(limit);
    }

    Ok(ConceptsResult { concepts })
}
