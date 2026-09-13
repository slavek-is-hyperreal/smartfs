//! FUSE runtime state management, file handles, in-memory buffers, and delayed deletion.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use fuser::{FileAttr, FileType};
use smartfs_db::{AstNodeInsert, InodeRecord};
use smartfs_schema::error::{Result, SmartFsError};
use uuid::Uuid;

/// @id: b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e
/// Convert an `InodeRecord` from `smartfs-db` into a FUSE `FileAttr`.
///
/// Invariants:
/// - Inode number is `u64` (from BIGSERIAL).
/// - Size is never negative (never cast -1 to u64).
/// - Blocks calculated using standard 512-byte POSIX units.
/// - Permissions masked from POSIX mode.
pub fn inode_to_file_attr(record: &InodeRecord) -> FileAttr {
    let size = record.size.max(0) as u64;
    let blocks = size.div_ceil(512);
    let kind = if record.is_dir {
        FileType::Directory
    } else {
        FileType::RegularFile
    };
    let perm = (record.mode as u16) & 0o7777;
    let nlink = if record.is_dir {
        2
    } else {
        record.nlink.max(1) as u32
    };

    let atime = system_time_from_datetime(&record.updated_at);
    let mtime = system_time_from_datetime(&record.updated_at);
    let ctime = system_time_from_datetime(&record.updated_at);
    let crtime = system_time_from_datetime(&record.created_at);

    FileAttr {
        ino: record.ino as u64,
        size,
        blocks,
        atime,
        mtime,
        ctime,
        crtime,
        kind,
        perm,
        nlink,
        uid: record.uid as u32,
        gid: record.gid as u32,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

/// @id: c3d4e5f6-a7b8-4c9d-0e1f-2a3b4c5d6e7f
/// Convert a `chrono::DateTime<Utc>` into a standard `SystemTime`.
pub fn system_time_from_datetime(dt: &DateTime<Utc>) -> SystemTime {
    let secs = dt.timestamp();
    let nsecs = dt.timestamp_subsec_nanos();
    if secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::new(secs as u64, nsecs)
    } else {
        SystemTime::UNIX_EPOCH - Duration::new((-secs) as u64, 0)
    }
}

/// @id: d4e5f6a7-b8c9-4d0e-1f2a-3b4c5d6e7f80
/// Open file handle descriptor tracking buffered content and AST analysis state.
#[derive(Debug, Clone)]
pub struct OpenHandle {
    /// Kernel-assigned file handle ID.
    pub fh: u64,
    /// FUSE inode number (u64).
    pub ino: u64,
    /// Internal primary key UUID in `inode_registry`.
    pub inode_id: Uuid,
    /// POSIX open flags (e.g. O_RDWR, O_APPEND, etc.).
    pub flags: i32,
    /// Random-access per-fd buffer (entire file in memory, §3.6 / ADR-16).
    pub buffer: Option<Vec<u8>>,
    /// Flag indicating whether the buffer was mutated by write or truncate operations.
    pub modified: bool,
    /// Cached AST nodes extracted during `flush()` for commit in `release()`.
    pub ast_nodes: Vec<AstNodeInsert>,
}

/// @id: e5f6a7b8-c9d0-4e1f-2a3b-4c5d6e7f8091
/// State tracking open file descriptor count and delayed deletion for an inode.
#[derive(Debug, Clone, Default)]
pub struct InodeState {
    /// Number of active open file descriptors for this inode.
    pub open_fd_count: usize,
    /// Whether this inode has been unlinked while still open by active file descriptors.
    pub marked_for_deletion: bool,
}

/// @id: f6a7b8c9-d0e1-4f2a-3b4c-5d6e7f8091a2
/// Concurrent thread-safe state manager for the FUSE daemon.
pub struct FuseStateManager {
    next_fh: AtomicU64,
    handles: RwLock<HashMap<u64, OpenHandle>>,
    inodes: RwLock<HashMap<Uuid, InodeState>>,
    ino_to_id: RwLock<HashMap<u64, Uuid>>,
}

/// @id: b8a75f66-9325-4f8f-8281-bb89cdf58980
impl Default for FuseStateManager {
    fn default() -> Self {
        Self::new()
    }
}

/// @id: 1eebeab2-9def-4f09-ba84-88ced7726b3e
impl FuseStateManager {
    /// @id: 07b8c9d0-e1f2-4a3b-4c5d-6e7f8091a2b3
    /// Creates a new, empty `FuseStateManager`.
    pub fn new() -> Self {
        Self {
            next_fh: AtomicU64::new(1),
            handles: RwLock::new(HashMap::new()),
            inodes: RwLock::new(HashMap::new()),
            ino_to_id: RwLock::new(HashMap::new()),
        }
    }

    /// @id: 18c9d0e1-f2a3-4b4c-5d6e-7f8091a2b3c4
    /// Allocate a new unique file handle, incrementing `open_fd_count` for the inode.
    pub fn allocate_fh(
        &self,
        ino: u64,
        inode_id: Uuid,
        flags: i32,
        initial_buffer: Option<Vec<u8>>,
    ) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::SeqCst);
        let modified = initial_buffer.is_some();
        let handle = OpenHandle {
            fh,
            ino,
            inode_id,
            flags,
            buffer: initial_buffer,
            modified,
            ast_nodes: Vec::new(),
        };

        {
            let mut handles = self.handles.write().expect("handles lock poisoned");
            handles.insert(fh, handle);
        }

        {
            let mut inodes = self.inodes.write().expect("inodes lock poisoned");
            let state = inodes.entry(inode_id).or_default();
            state.open_fd_count += 1;
        }

        {
            let mut mapping = self.ino_to_id.write().expect("ino_to_id lock poisoned");
            mapping.insert(ino, inode_id);
        }

        fh
    }

    /// @id: 29d0e1f2-a3b4-4c5d-6e7f-8091a2b3c4d5
    /// Retrieves a cloned snapshot of the `OpenHandle` for the given file handle.
    pub fn get_handle(&self, fh: u64) -> Option<OpenHandle> {
        let handles = self.handles.read().expect("handles lock poisoned");
        handles.get(&fh).cloned()
    }

    /// @id: 3ae1f2a3-b4c5-4d6e-7f80-91a2b3c4d5e6
    /// Ensure the buffer is loaded for `fh`, invoking `loader` if currently uninitialized.
    pub fn ensure_buffer<F>(&self, fh: u64, loader: F) -> Result<()>
    where
        F: FnOnce() -> Result<Vec<u8>>,
    {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        if let Some(h) = handles.get_mut(&fh) {
            if h.buffer.is_none() {
                let data = loader()?;
                h.buffer = Some(data);
            }
            Ok(())
        } else {
            Err(SmartFsError::NotFound(format!("File handle {fh} not found")))
        }
    }

    /// @id: 4bf2a3b4-c5d6-4e7f-8091-a2b3c4d5e6f7
    /// Writes raw bytes to the per-fd memory buffer at `offset`, expanding it if necessary.
    pub fn write_to_handle(&self, fh: u64, offset: usize, data: &[u8]) -> Result<usize> {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        let handle = handles
            .get_mut(&fh)
            .ok_or_else(|| SmartFsError::NotFound(format!("File handle {fh} not found")))?;

        let end = offset.saturating_add(data.len());
        if end > 2 * 1024 * 1024 * 1024 {
            return Err(SmartFsError::Io(std::io::Error::from_raw_os_error(libc::EFBIG)));
        }
        let buffer = handle.buffer.get_or_insert_with(Vec::new);
        if end > buffer.len() {
            buffer.resize(end, 0);
        }
        buffer[offset..end].copy_from_slice(data);
        handle.modified = true;
        Ok(data.len())
    }

    /// @id: 5c03b4c5-d6e7-4f80-91a2-b3c4d5e6f708
    /// Truncate or resize the per-fd memory buffer to `new_size` without creating a version.
    pub fn truncate_handle(&self, fh: u64, new_size: usize) -> Result<()> {
        if new_size > 2 * 1024 * 1024 * 1024 {
            return Err(SmartFsError::Io(std::io::Error::from_raw_os_error(libc::EFBIG)));
        }
        let mut handles = self.handles.write().expect("handles lock poisoned");
        let handle = handles
            .get_mut(&fh)
            .ok_or_else(|| SmartFsError::NotFound(format!("File handle {fh} not found")))?;

        let buffer = handle.buffer.get_or_insert_with(Vec::new);
        buffer.resize(new_size, 0);
        handle.modified = true;
        Ok(())
    }

    /// @id: 4122d20e-6f5e-4c9f-86f2-bb6c12d26458
    /// Truncate or resize the per-fd memory buffer for all open handles of an inode without committing.
    pub fn truncate_inode_handles(&self, inode_id: Uuid, new_size: usize) {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        for handle in handles.values_mut() {
            if handle.inode_id == inode_id {
                let buffer = handle.buffer.get_or_insert_with(Vec::new);
                buffer.resize(new_size, 0);
                handle.modified = true;
            }
        }
    }

    /// @id: 7162986c-7e61-4560-84a2-19e344e7c381
    /// Sets the per-fd memory buffer for all open handles of an inode to `data`, marking modified=true.
    pub fn truncate_inode_handles_with_data(&self, inode_id: Uuid, data: Vec<u8>) {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        for handle in handles.values_mut() {
            if handle.inode_id == inode_id {
                handle.buffer = Some(data.clone());
                handle.modified = true;
            }
        }
    }

    /// @id: 5c03b4c5-d6e7-4f80-91a2-b3c4d5e6f709
    /// Updates all open handles for an inode with newly committed truncated data, setting modified=false.
    pub fn truncate_inode_handles_committed(&self, inode_id: Uuid, data: &[u8]) {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        for handle in handles.values_mut() {
            if handle.inode_id == inode_id {
                handle.buffer = Some(data.to_vec());
                handle.modified = false;
            }
        }
    }

    /// @id: 5c03b4c5-d6e7-4f80-91a2-b3c4d5e6f70a
    /// Updates a specific open handle with newly committed truncated data, setting modified=false.
    pub fn truncate_handle_committed(&self, fh: u64, data: &[u8]) -> Result<()> {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        let handle = handles
            .get_mut(&fh)
            .ok_or_else(|| SmartFsError::NotFound(format!("File handle {fh} not found")))?;
        handle.buffer = Some(data.to_vec());
        handle.modified = false;
        Ok(())
    }

    /// @id: 6d14c5d6-e7f8-4091-a2b3-c4d5e6f70819
    /// Store AST nodes in per-fd state (extracted during `flush()`).
    pub fn store_ast_nodes(&self, fh: u64, nodes: Vec<AstNodeInsert>) {
        let mut handles = self.handles.write().expect("handles lock poisoned");
        if let Some(handle) = handles.get_mut(&fh) {
            handle.ast_nodes = nodes;
        }
    }

    /// @id: 7e25d6e7-f809-41a2-b3c4-d5e6f708192a
    /// Close a file handle, decrementing `open_fd_count`.
    /// Returns `(handle, should_delayed_delete)` where `should_delayed_delete == true`
    /// indicates that `open_fd_count` reached 0 and the inode was marked for deletion.
    pub fn close_handle(&self, fh: u64) -> Option<(OpenHandle, bool)> {
        let handle = {
            let mut handles = self.handles.write().expect("handles lock poisoned");
            handles.remove(&fh)?
        };

        let mut should_delete = false;
        {
            let mut inodes = self.inodes.write().expect("inodes lock poisoned");
            if let Some(state) = inodes.get_mut(&handle.inode_id) {
                state.open_fd_count = state.open_fd_count.saturating_sub(1);
                if state.open_fd_count == 0 {
                    if state.marked_for_deletion {
                        should_delete = true;
                    }
                    inodes.remove(&handle.inode_id);
                }
            }
        }

        Some((handle, should_delete))
    }

    /// @id: 8f36e7f8-091a-42b3-c4d5-e6f708192a3b
    /// Mark an inode for deletion upon unlink or rmdir.
    /// Returns `true` if `open_fd_count == 0` (immediate deletion allowed).
    /// Returns `false` if `open_fd_count > 0` (deletion must be delayed until `release()`).
    pub fn mark_for_deletion(&self, inode_id: Uuid) -> bool {
        let mut inodes = self.inodes.write().expect("inodes lock poisoned");
        let state = inodes.entry(inode_id).or_default();
        if state.open_fd_count == 0 {
            inodes.remove(&inode_id);
            true
        } else {
            state.marked_for_deletion = true;
            false
        }
    }

    /// @id: 9a47f809-1a2b-43c4-d5e6-f708192a3b4c
    /// Checks whether an inode is marked for deletion.
    pub fn is_marked_for_deletion(&self, inode_id: Uuid) -> bool {
        let inodes = self.inodes.read().expect("inodes lock poisoned");
        inodes
            .get(&inode_id)
            .map(|s| s.marked_for_deletion)
            .unwrap_or(false)
    }

    /// @id: ab58091a-2b3c-44d5-e6f7-08192a3b4c5d
    /// Returns the number of open file descriptors for the given inode.
    pub fn open_fd_count(&self, inode_id: Uuid) -> usize {
        let inodes = self.inodes.read().expect("inodes lock poisoned");
        inodes.get(&inode_id).map(|s| s.open_fd_count).unwrap_or(0)
    }

    /// @id: bc691a2b-3c4d-45e6-f708-192a3b4c5d6e
    /// Caches the mapping between FUSE `ino` and database UUID.
    pub fn cache_ino_mapping(&self, ino: u64, id: Uuid) {
        let mut mapping = self.ino_to_id.write().expect("ino_to_id lock poisoned");
        mapping.insert(ino, id);
    }

    /// @id: cd7a2b3c-4d5e-46f7-0819-2a3b4c5d6e7f
    /// Looks up a cached database UUID for the given FUSE `ino`.
    pub fn get_cached_id(&self, ino: u64) -> Option<Uuid> {
        let mapping = self.ino_to_id.read().expect("ino_to_id lock poisoned");
        mapping.get(&ino).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_inode_to_file_attr_conversion() {
        let now = Utc.with_ymd_and_hms(2026, 9, 12, 18, 0, 0).unwrap();
        let record = InodeRecord {
            id: Uuid::new_v4(),
            ino: 42,
            parent_id: None,
            name: "main.rs".to_string(),
            is_dir: false,
            uid: 1000,
            gid: 1000,
            mode: 0o100644,
            size: 1024,
            nlink: 1,
            created_at: now,
            updated_at: now,
            current_blob_id: None,
            backend_id: None,
            on_prem: true,
            compression_level: 3,
            versioning_enabled: true,
        };

        let attr = inode_to_file_attr(&record);
        assert_eq!(attr.ino, 42);
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.blocks, 2); // 1024 bytes / 512 = 2 blocks
        assert_eq!(attr.kind, FileType::RegularFile);
        assert_eq!(attr.perm, 0o644);
        assert_eq!(attr.uid, 1000);
        assert_eq!(attr.gid, 1000);
        assert_eq!(attr.blksize, 4096);
    }

    #[test]
    fn test_negative_size_safeguard() {
        let now = Utc::now();
        let record = InodeRecord {
            id: Uuid::new_v4(),
            ino: 100,
            parent_id: None,
            name: "corrupted_size".to_string(),
            is_dir: false,
            uid: 1000,
            gid: 1000,
            mode: 0o644,
            size: -1, // corrupted/invalid negative size
            nlink: 1,
            created_at: now,
            updated_at: now,
            current_blob_id: None,
            backend_id: None,
            on_prem: true,
            compression_level: 3,
            versioning_enabled: true,
        };

        let attr = inode_to_file_attr(&record);
        assert_eq!(attr.size, 0, "Negative size must be safely clamped to 0");
    }

    #[test]
    fn test_file_handle_state_and_buffer() {
        let manager = FuseStateManager::new();
        let inode_id = Uuid::new_v4();
        let fh = manager.allocate_fh(10, inode_id, 0, None);
        assert_eq!(fh, 1);
        assert_eq!(manager.open_fd_count(inode_id), 1);

        // Ensure buffer initialized
        manager
            .ensure_buffer(fh, || Ok(b"initial content".to_vec()))
            .unwrap();
        let h = manager.get_handle(fh).unwrap();
        assert_eq!(h.buffer.unwrap(), b"initial content");

        // Write random-access at offset
        let written = manager.write_to_handle(fh, 8, b"WORLD!!").unwrap();
        assert_eq!(written, 7);
        let h = manager.get_handle(fh).unwrap();
        assert_eq!(h.buffer.unwrap(), b"initial WORLD!!");
        assert!(h.modified);

        // Truncate
        manager.truncate_handle(fh, 7).unwrap();
        let h = manager.get_handle(fh).unwrap();
        assert_eq!(h.buffer.unwrap(), b"initial");

        // Store AST nodes
        let ast = vec![AstNodeInsert {
            kind: "function".into(),
            name: "test_fn".into(),
            start_line: 1,
            end_line: 5,
            source: "fn test_fn() {}".into(),
            content_hash: "abcd".into(),
        }];
        manager.store_ast_nodes(fh, ast.clone());
        let h = manager.get_handle(fh).unwrap();
        assert_eq!(h.ast_nodes.len(), 1);

        // Close handle
        let (closed, should_delete) = manager.close_handle(fh).unwrap();
        assert_eq!(closed.fh, fh);
        assert!(!should_delete);
        assert_eq!(manager.open_fd_count(inode_id), 0);
    }

    #[test]
    fn test_delayed_unlink_workflow() {
        let manager = FuseStateManager::new();
        let inode_id = Uuid::new_v4();

        // 1. Open two file descriptors on the same inode
        let fh1 = manager.allocate_fh(20, inode_id, 0, None);
        let fh2 = manager.allocate_fh(20, inode_id, 0, None);
        assert_eq!(manager.open_fd_count(inode_id), 2);

        // 2. Unlink while open: deletion must be delayed
        let immediate_delete = manager.mark_for_deletion(inode_id);
        assert!(
            !immediate_delete,
            "Must NOT delete immediately while open_fd_count > 0"
        );
        assert!(manager.is_marked_for_deletion(inode_id));

        // 3. Close first fd: still 1 open, should NOT delete yet
        let (_, should_delete1) = manager.close_handle(fh1).unwrap();
        assert!(!should_delete1);
        assert_eq!(manager.open_fd_count(inode_id), 1);

        // 4. Close last fd: open_fd_count reaches 0 -> should delete NOW
        let (_, should_delete2) = manager.close_handle(fh2).unwrap();
        assert!(
            should_delete2,
            "Must trigger delayed deletion when last fd is released"
        );
        assert_eq!(manager.open_fd_count(inode_id), 0);
    }

    #[test]
    fn test_immediate_unlink_when_no_open_fds() {
        let manager = FuseStateManager::new();
        let inode_id = Uuid::new_v4();

        // Inode has 0 open file descriptors
        let immediate_delete = manager.mark_for_deletion(inode_id);
        assert!(
            immediate_delete,
            "Must delete immediately when open_fd_count == 0"
        );
    }
}
