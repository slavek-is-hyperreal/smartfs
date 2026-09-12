//! Mount helper functions and standard mount option defaults for SmartFS FUSE daemon.

use std::path::Path;
use fuser::{BackgroundSession, MountOption};
use smartfs_schema::error::Result;

use crate::fs::SmartFsFuse;

/// @id: 23d08192-a3b4-4c56-6d7e-8f90a1b2c3d4
/// Returns the canonical mount options required for the SmartFS daemon (§3.6):
/// - `MountOption::DefaultPermissions`: kernel enforces uid/gid/mode permissions
/// - `MountOption::AllowOther`: mountpoint visible to all users
pub fn default_mount_options() -> Vec<MountOption> {
    vec![
        MountOption::DefaultPermissions,
        MountOption::AllowOther,
        MountOption::FSName("smartfs".to_string()),
        MountOption::AutoUnmount,
    ]
}

/// @id: 34e192a3-b4c5-4d67-7e8f-90a1b2c3d4e5
/// Mount the SmartFS filesystem synchronously at `mountpoint`.
/// Does not return until unmounted.
pub fn mount_smartfs(
    fs: SmartFsFuse,
    mountpoint: impl AsRef<Path>,
    options: &[MountOption],
) -> Result<()> {
    fuser::mount2(fs, mountpoint, options)?;
    Ok(())
}

/// @id: 45f2a3b4-c5d6-4e78-8f90-a1b2c3d4e5f6
/// Mount the SmartFS filesystem asynchronously in a background thread.
/// Returns a `BackgroundSession` handle which unmounts the filesystem when dropped.
pub fn spawn_mount_smartfs(
    fs: SmartFsFuse,
    mountpoint: impl AsRef<Path>,
    options: &[MountOption],
) -> Result<BackgroundSession> {
    let session = fuser::spawn_mount2(fs, mountpoint, options)?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_mount_options_contains_required_flags() {
        let opts = default_mount_options();
        assert!(opts.contains(&MountOption::DefaultPermissions));
        assert!(opts.contains(&MountOption::AllowOther));
    }
}
