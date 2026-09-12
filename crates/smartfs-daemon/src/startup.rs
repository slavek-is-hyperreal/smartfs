//! Startup gates for `smartfsd`, in the exact order specified by
//! docs/testing/the-great-smartfs-test.md §1.3.
//!
//! Every function here is a hard gate. On failure the caller logs the error
//! chain at `ERROR` and exits with the code named in [`crate::exit`]; nothing
//! here falls back to a degraded mode and nothing here retries forever.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use smartfs_db::PgPool;
use smartfs_store::{BlobStore, LocalDiskStore};
use uuid::Uuid;

/// @id: bc052028-c06b-4753-811e-3d3664c07ee8
/// The four core tables migration `001` creates. Their absence is a hard stop.
pub const CORE_TABLES: [&str; 4] = [
    "inode_registry",
    "file_versions",
    "blobs",
    "storage_backends",
];

/// @id: 7fbfeb8e-af60-454a-bf29-b63611cc1f08
/// Evidence that a migration has been applied, in the absence of a ledger table.
///
/// The schema carries no migrations ledger (Root Invariant #4 forbids creating
/// one at runtime), so "recorded as applied" is checked against the catalog
/// objects each migration file creates. `column` is `None` for a table marker.
pub struct MigrationMarker {
    /// Migration number, `001` .. `006`.
    pub id: &'static str,
    /// A table the migration creates, or the table a marker column lives on.
    pub table: &'static str,
    /// A column the migration adds, when a whole table is not distinctive.
    pub column: Option<&'static str>,
}

/// @id: 19443a9e-1ea7-4fef-b2cb-6be74c03340c
/// The `001`–`006` markers, derived from `migrations/0*.sql`.
pub const MIGRATION_MARKERS: [MigrationMarker; 6] = [
    MigrationMarker { id: "001", table: "inode_registry", column: None },
    MigrationMarker { id: "002", table: "embedding_models", column: None },
    MigrationMarker { id: "003", table: "embeddings_768", column: None },
    MigrationMarker { id: "004", table: "ast_nodes", column: None },
    MigrationMarker { id: "005", table: "consolidation_thresholds", column: None },
    MigrationMarker { id: "006", table: "file_versions", column: Some("search_text") },
];

/// @id: 70559553-7e64-4d66-8158-916855455173
/// Step 3 — validate the mountpoint: exists, is a directory, is empty, and is
/// not already a mount according to `/proc/self/mountinfo`.
pub fn validate_mountpoint(mountpoint: &Path) -> Result<PathBuf> {
    let meta = fs::metadata(mountpoint)
        .with_context(|| format!("mountpoint {} does not exist or is not stat-able", mountpoint.display()))?;
    if !meta.is_dir() {
        bail!("mountpoint {} is not a directory", mountpoint.display());
    }

    let canonical = fs::canonicalize(mountpoint)
        .with_context(|| format!("cannot canonicalize mountpoint {}", mountpoint.display()))?;

    let mut entries = fs::read_dir(&canonical)
        .with_context(|| format!("cannot read mountpoint {}", canonical.display()))?;
    if let Some(first) = entries.next() {
        let name = first
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .unwrap_or_else(|_| "<unreadable>".to_string());
        bail!(
            "mountpoint {} is not empty (contains {name:?}); FUSE would hide its contents",
            canonical.display()
        );
    }

    if is_mountpoint(&canonical)? {
        bail!(
            "mountpoint {} is already a mount; unmount it before starting smartfsd",
            canonical.display()
        );
    }

    Ok(canonical)
}

/// @id: 5ee9797e-4de5-419e-8143-6eb82d8c878b
/// Reports whether `path` appears as a mount target in `/proc/self/mountinfo`.
pub fn is_mountpoint(path: &Path) -> Result<bool> {
    Ok(mountinfo_fstype(path)?.is_some())
}

/// @id: f8e8c731-dd6c-4a83-960e-afa6cbad8b0a
/// Returns the filesystem type `/proc/self/mountinfo` records for `path`, if it
/// is a mount target at all.
///
/// mountinfo fields: `id parent major:minor root MOUNTPOINT opts... - FSTYPE source super_opts`.
/// The mountpoint field is octal-escaped for space, tab, newline and backslash.
pub fn mountinfo_fstype(path: &Path) -> Result<Option<String>> {
    let target = path.to_string_lossy();
    let content = fs::read_to_string("/proc/self/mountinfo")
        .context("cannot read /proc/self/mountinfo")?;

    let mut found = None;
    for line in content.lines() {
        let mut parts = line.split(' ');
        let mount_point = match parts.nth(4) {
            Some(m) => unescape_mountinfo(m),
            None => continue,
        };
        if mount_point != target {
            continue;
        }
        // Everything after the " - " separator: fstype, source, super options.
        let fstype = line
            .split(" - ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        // Later entries shadow earlier ones for the same mountpoint.
        found = Some(fstype);
    }
    Ok(found)
}

/// @id: 439fe18e-9f40-4a63-af08-09f6c35aad22
/// Decodes the octal escapes `/proc/self/mountinfo` uses in path fields.
pub fn unescape_mountinfo(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &s[i + 1..i + 4];
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                out.push(v as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// @id: 994d5240-5f6f-4088-9b69-50e5c9db2d59
/// Step 4 — validate the store path: exists, is a directory, is writable
/// (proven by creating and deleting a probe file, not by inspecting the mode).
pub fn validate_store_path(store_path: &Path) -> Result<PathBuf> {
    let meta = fs::metadata(store_path).with_context(|| {
        format!(
            "store path {} does not exist. smartfsd never creates it — \
             a missing blob store means the wrong path was configured",
            store_path.display()
        )
    })?;
    if !meta.is_dir() {
        bail!("store path {} is not a directory", store_path.display());
    }

    let probe = store_path.join(format!(".smartfsd-write-probe-{}", std::process::id()));
    fs::write(&probe, b"smartfsd write probe")
        .with_context(|| format!("store path {} is not writable", store_path.display()))?;
    fs::remove_file(&probe)
        .with_context(|| format!("cannot remove write probe {}", probe.display()))?;

    fs::canonicalize(store_path)
        .with_context(|| format!("cannot canonicalize store path {}", store_path.display()))
}

/// @id: 17f3d08d-9fed-4c79-bd01-3e802800f975
/// Step 5 — connect the pool and prove the connection with `SELECT 1`.
///
/// No `Err(_) => return`, and no retry-forever loop that hides an unreachable
/// database: this returns `Err` and the caller exits 4.
pub async fn connect_and_probe_db(database_url: &str) -> Result<PgPool> {
    let pool = smartfs_db::connect_pool(database_url)
        .await
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| "cannot connect to PostgreSQL")?;

    smartfs_db::ping(&pool)
        .await
        .map_err(|e| anyhow!("{e}"))
        .context("connected to PostgreSQL but 'SELECT 1' failed")?;

    Ok(pool)
}

/// @id: 89a09fc1-d247-4ee3-a1e2-c33dd51f7b74
/// Step 6 — schema sanity check. Never issues DDL (Root Invariant #4).
///
/// Asserts the four core tables exist, that migrations `001`–`006` are recorded
/// as applied, and that `blobs` has no `refcount` column (its presence means
/// FIX-01 was re-introduced).
pub async fn check_schema(pool: &PgPool) -> Result<()> {
    let mut missing = Vec::new();
    for table in CORE_TABLES {
        if !table_exists(pool, table).await? {
            missing.push(table);
        }
    }
    if !missing.is_empty() {
        bail!(
            "missing core tables: {}. Apply migrations/001_core_schema.sql .. \
             006_fulltext_search.sql in order, as files. Nothing may create them \
             at runtime — that would violate Root Invariant #4",
            missing.join(", ")
        );
    }

    let mut unapplied = Vec::new();
    for marker in &MIGRATION_MARKERS {
        let present = match marker.column {
            Some(column) => column_exists(pool, marker.table, column).await?,
            None => table_exists(pool, marker.table).await?,
        };
        if !present {
            unapplied.push(marker.id);
        }
    }
    if !unapplied.is_empty() {
        bail!(
            "migration(s) {} are not applied to this database. Apply \
             migrations/0*.sql in order and restart; smartfsd will not create schema",
            unapplied.join(", ")
        );
    }

    if column_exists(pool, "blobs", "refcount").await? {
        bail!(
            "blobs.refcount exists — FIX-01 has been re-introduced. \
             SmartFS dedup is refcount-free; refusing to serve against this schema"
        );
    }

    tracing::info!(
        migrations = "001-006",
        "schema sanity check passed (core tables present, no blobs.refcount)"
    );
    Ok(())
}

/// @id: c49bffc6-0811-4730-8733-4846931569c6
/// Reports whether `table` exists in the `public` schema.
pub async fn table_exists(pool: &PgPool, table: &str) -> Result<bool> {
    smartfs_db::table_exists(pool, table)
        .await
        .map_err(|e| anyhow!("information_schema.tables lookup for {table} failed: {e}"))
}

/// @id: 3dae6c4f-f707-4bcd-aac9-f2976b992e2a
/// Reports whether `table.column` exists in the `public` schema.
pub async fn column_exists(pool: &PgPool, table: &str, column: &str) -> Result<bool> {
    smartfs_db::column_exists(pool, table, column)
        .await
        .map_err(|e| anyhow!("information_schema.columns lookup for {table}.{column} failed: {e}"))
}

/// @id: caab43f9-6cbd-4ed5-8ad3-b080f1f2796f
/// Step 7 — open the blob store handle and prove it can reach the backend.
///
/// `LocalDiskStore::new` cannot fail on its own, so the gate is a real backend
/// round-trip: an `exists()` lookup for a UUID that is not there must answer
/// `false` rather than raise an I/O error.
pub async fn open_store(store_path: &Path) -> Result<LocalDiskStore> {
    let store = LocalDiskStore::new(store_path);
    let probe = Uuid::new_v4();
    let present = store
        .exists(probe)
        .await
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("blob store at {} is not usable", store_path.display()))?;
    if present {
        bail!("blob store reported a freshly generated UUID {probe} as already present");
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn empty_directory_is_a_valid_mountpoint() {
        let dir = tempdir().unwrap();
        assert!(validate_mountpoint(dir.path()).is_ok());
    }

    #[test]
    fn non_empty_directory_is_rejected() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("stray")).unwrap();
        let err = validate_mountpoint(dir.path()).unwrap_err().to_string();
        assert!(err.contains("not empty"), "unexpected error: {err}");
    }

    #[test]
    fn missing_mountpoint_is_rejected() {
        let dir = tempdir().unwrap();
        assert!(validate_mountpoint(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn file_is_not_a_valid_mountpoint() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("file");
        File::create(&f).unwrap();
        let err = validate_mountpoint(&f).unwrap_err().to_string();
        assert!(err.contains("not a directory"), "unexpected error: {err}");
    }

    #[test]
    fn writable_directory_is_a_valid_store_path() {
        let dir = tempdir().unwrap();
        assert!(validate_store_path(dir.path()).is_ok());
        // The probe file must not survive the check.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn missing_store_path_is_rejected() {
        let dir = tempdir().unwrap();
        assert!(validate_store_path(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn root_is_a_mountpoint_and_a_temp_dir_is_not() {
        assert!(is_mountpoint(Path::new("/")).unwrap());
        let dir = tempdir().unwrap();
        assert!(!is_mountpoint(dir.path()).unwrap());
    }

    #[test]
    fn mountinfo_escapes_are_decoded() {
        assert_eq!(unescape_mountinfo(r"/mnt/a\040b"), "/mnt/a b");
        assert_eq!(unescape_mountinfo("/mnt/plain"), "/mnt/plain");
    }

    #[tokio::test]
    async fn store_opens_against_a_writable_directory() {
        let dir = tempdir().unwrap();
        assert!(open_store(dir.path()).await.is_ok());
    }
}
