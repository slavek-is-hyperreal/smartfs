//! Exit code definitions for `smartfsd`, conforming to
//! docs/testing/the-great-smartfs-test.md §1.3.

/// @id: a1b2c3d4-e5f6-4789-a012-3456789abcde
/// Exit code on clean shutdown (e.g. SIGINT/SIGTERM).
pub const SUCCESS: i32 = 0;

/// @id: b2c3d4e5-f6a7-4890-b123-456789abcdef
/// Mountpoint does not exist, is not a directory, is not empty, or is already mounted.
pub const MOUNTPOINT_INVALID: i32 = 2;

/// @id: c3d4e5f6-a7b8-4901-c234-56789abcdef0
/// Store path does not exist, is not a directory, or is not writable.
pub const STORE_PATH_INVALID: i32 = 3;

/// @id: d4e5f6a7-b8c9-4012-d345-6789abcdef01
/// Connecting to PostgreSQL or 'SELECT 1' probe failed.
pub const DB_CONNECT_FAILED: i32 = 4;

/// @id: e5f6a7b8-c9d0-4123-e456-789abcdef012
/// Core tables missing, migrations 001-006 unapplied, or blobs.refcount exists.
pub const SCHEMA_CHECK_FAILED: i32 = 5;

/// @id: f6a7b8c9-d0e1-4234-f567-89abcdef0123
/// Opening local disk blob store failed or probe returned unexpected state.
pub const STORE_OPEN_FAILED: i32 = 6;

/// @id: 07b8c9d0-e1f2-4345-a678-9abcdef01234
/// Spawning FUSE mount failed or verifying mount stat failed.
pub const MOUNT_FAILED: i32 = 7;
