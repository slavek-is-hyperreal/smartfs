//! Handler for `search` command.

use smartfs_db::{search_fulltext_bm25, FulltextHit, PgPool};
use smartfs_schema::error::Result;

use crate::args::SearchArgs;

/// @id: e6a1b2c3-3004-4000-8000-000000000001
/// Result of the `search` command.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub query: String,
    pub hits: Vec<FulltextHit>,
}

/// @id: e6a1b2c3-3004-4000-8000-000000000002
/// Handles execution of the `search` command.
pub async fn handle_search(
    pool: &PgPool,
    args: &SearchArgs,
) -> Result<SearchResult> {
    let hits = search_fulltext_bm25(
        pool,
        &args.query,
        args.plugin_type.as_deref(),
        args.limit,
    )
    .await?;

    Ok(SearchResult {
        query: args.query.clone(),
        hits,
    })
}
