//! Handler for `diff` command.

use std::collections::HashMap;
use smartfs_db::{get_ast_nodes, version_get, AstNodeRecord, PgPool};
use smartfs_schema::error::{Result, SmartFsError};

use crate::args::DiffArgs;
use crate::path::resolve_path;

/// @id: e6a1b2c3-3005-4000-8000-000000000001
/// Result of the `diff` command showing AST differences between versions.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffResult {
    pub path: String,
    pub v1: i32,
    pub v2: i32,
    pub added: Vec<AstNodeRecord>,
    pub changed: Vec<AstNodeRecord>,
    pub removed: Vec<AstNodeRecord>,
}

/// @id: e6a1b2c3-3005-4000-8000-000000000002
/// Handles execution of the `diff` command.
pub async fn handle_diff(
    pool: &PgPool,
    args: &DiffArgs,
) -> Result<DiffResult> {
    let inode = resolve_path(pool, &args.path).await?;

    let rec1 = version_get(pool, inode.id, Some(args.v1))
        .await?
        .ok_or_else(|| {
            SmartFsError::NotFound(format!("Version {} not found for '{}'", args.v1, args.path))
        })?;

    let rec2 = version_get(pool, inode.id, Some(args.v2))
        .await?
        .ok_or_else(|| {
            SmartFsError::NotFound(format!("Version {} not found for '{}'", args.v2, args.path))
        })?;

    let nodes1 = get_ast_nodes(pool, rec1.id).await?;
    let nodes2 = get_ast_nodes(pool, rec2.id).await?;

    let mut map1: HashMap<(String, String), &AstNodeRecord> = HashMap::new();
    for n in &nodes1 {
        map1.insert((n.kind.clone(), n.name.clone()), n);
    }

    let mut map2: HashMap<(String, String), &AstNodeRecord> = HashMap::new();
    for n in &nodes2 {
        map2.insert((n.kind.clone(), n.name.clone()), n);
    }

    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();

    for (k, n2) in &map2 {
        match map1.get(k) {
            None => added.push((*n2).clone()),
            Some(n1) => {
                if n1.content_hash != n2.content_hash || n1.source != n2.source {
                    changed.push((*n2).clone());
                }
            }
        }
    }

    for (k, n1) in &map1 {
        if !map2.contains_key(k) {
            removed.push((*n1).clone());
        }
    }

    // Sort deterministic by start_line
    added.sort_by_key(|n| n.start_line);
    changed.sort_by_key(|n| n.start_line);
    removed.sort_by_key(|n| n.start_line);

    Ok(DiffResult {
        path: args.path.clone(),
        v1: args.v1,
        v2: args.v2,
        added,
        changed,
        removed,
    })
}
