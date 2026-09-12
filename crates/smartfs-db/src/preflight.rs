//! Connection liveness and schema-catalog introspection.
//!
//! Read-only by construction: nothing here issues DDL (Root Invariant #4).
//! Used by `smartfsd`'s startup gates (docs/testing/the-great-smartfs-test.md
//! §1.3 steps 5 and 6) so the daemon never has to inline SQL of its own.

use smartfs_schema::error::{Result, SmartFsError};
use sqlx::PgPool;

/// @id: ea28e679-dae7-4b4a-b7c0-317899d21aeb
/// Proves a pooled connection is live by running `SELECT 1`.
///
/// Returns `Err` when the database is unreachable or answers with anything but
/// `1`. Callers must treat that as fatal: an unreachable database is a failure,
/// never a degraded mode.
pub async fn ping(pool: &PgPool) -> Result<()> {
    let one: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| SmartFsError::Db(format!("SELECT 1 failed: {e}")))?;
    if one != 1 {
        return Err(SmartFsError::Db(format!("SELECT 1 returned {one}, not 1")));
    }
    Ok(())
}

/// @id: fad97f5f-08c2-484c-b557-06a3a9dc8889
/// Reports whether `table` exists in the `public` schema.
pub async fn table_exists(pool: &PgPool, table: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM information_schema.tables
        WHERE table_schema = 'public' AND table_name = $1
        "#,
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("table_exists({table}) error: {e}")))?;
    Ok(n > 0)
}

/// @id: 07907ab1-40b3-4d73-afea-1c57b1bbb85e
/// Reports whether `table`.`column` exists in the `public` schema.
pub async fn column_exists(pool: &PgPool, table: &str, column: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*) FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2
        "#,
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await
    .map_err(|e| SmartFsError::Db(format!("column_exists({table}.{column}) error: {e}")))?;
    Ok(n > 0)
}
