//! Core FUSE operation handlers translating VFS requests into smartfs-db and smartfs-store operations.
//!
//! Owns exclusively FUSE operation handlers (§3.6).
//! Strictly enforces:
//! - Root Invariant #1: content_hash calculated on original bytes BEFORE compression.
//! - Inode Number Mapping: FUSE kernel uses u64 ino. Schema uses BIGSERIAL ino column. Root = 1.
//! - Random-access RAM buffer: hash + compress + store ONLY in `release()`, never in `write()`.
//! - flush vs release pipeline (§11.1):
//!   - `flush()`: throwaway AST parse returning `EACCES` on syntax error if not forced.
//!   - `release()`: atomic CoW commit (`cow_commit`), FIX-03 healing, FIX-04 compensation.
//! - Delayed Unlink (§3.6, ADR-16): delays CASCADE delete until `open_fd_count` reaches 0.
//! - No SQL directly — delegates exclusively to `smartfs-db` public API.

use std::ffi::{CString, OsStr};
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use smartfs_db::PgPool;
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::BlobStore;
use uuid::Uuid;

use crate::error::error_to_errno;
use crate::pending::PendingPipeline;
use crate::state::{inode_to_file_attr, FuseStateManager};

/// Attribute / Entry cache TTL for FUSE operations.
pub const TTL: Duration = Duration::from_secs(1);

/// @id: 5603b4c5-d6e7-4f89-90a1-b2c3d4e5f607
/// Main SmartFS FUSE filesystem service implementation.
#[derive(Clone)]
pub struct SmartFsFuse {
    pool: PgPool,
    store: Arc<dyn BlobStore + Send + Sync>,
    blob_dir: PathBuf,
    rt_handle: tokio::runtime::Handle,
    state: Arc<FuseStateManager>,
    force: bool,
    /// Two-stage commit pipeline (ADR-58). There is deliberately no
    /// synchronous fallback: a filesystem with two write paths is a filesystem
    /// whose crash behaviour depends on how it was constructed.
    pending: PendingPipeline,
}

/// @id: 85d0907f-cba5-4631-8aa8-53f612c0192f
impl SmartFsFuse {
    /// @id: 6714c5d6-e7f8-409a-a1b2-c3d4e5f60718
    /// Creates a new `SmartFsFuse` filesystem instance.
    pub fn new(
        pool: PgPool,
        store: Arc<dyn BlobStore + Send + Sync>,
        blob_dir: impl AsRef<Path>,
        rt_handle: tokio::runtime::Handle,
        force: bool,
        pending: PendingPipeline,
    ) -> Self {
        Self {
            pool,
            store,
            blob_dir: blob_dir.as_ref().to_path_buf(),
            rt_handle,
            state: Arc::new(FuseStateManager::new()),
            force,
            pending,
        }
    }

    /// @id: 8c5e1a04-3d2b-4f77-91ce-6b0a4d9e7f13
    /// Returns the pending pipeline, for the daemon's shutdown drain and for
    /// status reporting.
    pub fn pending(&self) -> &PendingPipeline {
        &self.pending
    }

    /// @id: 9e4a1c73-05bd-42f6-b8e1-7c30a5d92b48
    /// Loads a file's current bytes: the newest uncommitted write if one is
    /// queued, otherwise what `inode_registry` points at.
    ///
    /// Shared by `read` and `write` so the two cannot disagree about what a
    /// file currently contains. The overlay lookup is the same one `read` and
    /// `getattr` use (ADR-58 point 7); before this existed, `write`'s own
    /// loader consulted only the database and could therefore seed its buffer
    /// from the *previous* version of a file whose newer write had been
    /// acknowledged but not yet drained — silently reverting it on the next
    /// flush.
    fn load_current_bytes(&self, ino: u64) -> Result<Vec<u8>> {
        let pool = self.pool.clone();
        let store = self.store.clone();
        let pending = self.pending.clone();

        self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

            let blob = match pending.view_of(record.id) {
                Some(view) => view.blob_id,
                None => record.current_blob_id,
            };

            match blob {
                Some(blob_id) => {
                    let compressed = store.get(blob_id, None).await?;
                    Ok(smartfs_compress::decompress(&compressed).unwrap_or(compressed))
                }
                None => Ok(Vec::new()),
            }
        })
    }

    /// @id: b40f7a92-1c6e-4d38-85a0-e2947fb60cd1
    /// Records an access for `relatime`, off the reply path (ADR-61 point 6).
    ///
    /// Spawned rather than awaited on purpose: a read that waits on a Postgres
    /// UPDATE to record that it happened is worse than an `atime` a few
    /// milliseconds late. The `relatime` predicate lives in SQL, so most calls
    /// update no rows at all and cost one cheap statement.
    ///
    /// Failure is logged and dropped. `atime` is advisory metadata; losing an
    /// update must never turn a successful read into an error.
    fn note_access(&self, inode_id: Uuid) {
        let pool = self.pool.clone();
        self.rt_handle.spawn(async move {
            if let Err(e) = smartfs_db::inode_touch_atime_relatime(&pool, inode_id).await {
                tracing::debug!("relatime update for {inode_id} failed, ignoring: {e}");
            }
        });
    }

    /// @id: 7825d6e7-f809-41ab-b2c3-d4e5f6071829
    /// Returns a reference to the shared `FuseStateManager`.
    pub fn state(&self) -> &Arc<FuseStateManager> {
        &self.state
    }

    /// Runs an asynchronous future synchronously, adapting between runtime contexts safely.
    fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(|| self.rt_handle.block_on(fut))
                }
                _ => {
                    let rt = self.rt_handle.clone();
                    std::thread::spawn(move || rt.block_on(fut))
                        .join()
                        .expect("FUSE async worker thread panicked")
                }
            }
        } else {
            self.rt_handle.block_on(fut)
        }
    }
}

/// @id: 107de35e-41a2-4ce0-baab-b6a616fc4797
impl Filesystem for SmartFsFuse {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let state = self.state.clone();
        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let parent_id = if parent == fuser::FUSE_ROOT_ID {
                let root = smartfs_db::inode_lookup_by_ino(&pool, 1)
                    .await?
                    .ok_or_else(|| SmartFsError::NotFound("Root inode (ino=1) not found".into()))?;
                state.cache_ino_mapping(1, root.id);
                root.id
            } else {
                match state.get_cached_id(parent) {
                    Some(id) => id,
                    None => {
                        let p = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                            .await?
                            .ok_or_else(|| {
                                SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                            })?;
                        state.cache_ino_mapping(parent, p.id);
                        p.id
                    }
                }
            };

            let child = smartfs_db::inode_lookup(&pool, Some(parent_id), &name_str)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Entry '{name_str}' not found")))?;

            if state.is_marked_for_deletion(child.id) {
                return Err(SmartFsError::NotFound(format!("Entry '{name_str}' unlinked")));
            }

            state.cache_ino_mapping(child.ino as u64, child.id);
            Ok(child)
        });

        match res {
            Ok(record) => {
                let mut attr = inode_to_file_attr(&record);
                if let Some(view) = self.pending.view_of(record.id) {
                    attr.size = view.size.max(0) as u64;
                    attr.blocks = attr.size.div_ceil(512);
                }
                reply.entry(&TTL, &attr, 1);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        let pool = self.pool.clone();
        let state = self.state.clone();
        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;
            state.cache_ino_mapping(ino, record.id);
            Ok(record)
        });

        match res {
            Ok(record) => {
                let mut attr = inode_to_file_attr(&record);
                // ADR-58: report the size of a write that has been
                // acknowledged but not yet committed, or stat() would contradict
                // the write() that just returned.
                if let Some(view) = self.pending.view_of(record.id) {
                    attr.size = view.size.max(0) as u64;
                    attr.blocks = attr.size.div_ceil(512);
                }
                if let Some(h_id) = fh {
                    if let Some(handle) = self.state.get_handle(h_id) {
                        if let Some(buf) = &handle.buffer {
                            attr.size = buf.len() as u64;
                            attr.blocks = attr.size.div_ceil(512);
                        }
                    }
                } else if let Some(buf_len) = self.state.get_inode_buffer_len(record.id) {
                    attr.size = buf_len as u64;
                    attr.blocks = attr.size.div_ceil(512);
                }
                reply.attr(&TTL, &attr);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if let Some(new_size) = size {
            if new_size > 2 * 1024 * 1024 * 1024 {
                reply.error(libc::EFBIG);
                return;
            }
        }

        let pool = self.pool.clone();
        let store = self.store.clone();
        let state = self.state.clone();
        let state_inner = state.clone();
        let pending = self.pending.clone();

        let res: Result<(smartfs_db::InodeRecord, Option<Vec<u8>>)> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

            let mut truncated_data = None;

            if let Some(new_size) = size {
                let has_open_handles = fh.is_some() || state_inner.open_fd_count(record.id) > 0;
                if has_open_handles {
                    // Architecture §11.2a: setattr with FATTR_SIZE on open file does NOT commit a version.
                    // It only truncates the per-fd memory buffer. The version is created in release().
                    if new_size == 0 {
                        if let Some(h_id) = fh {
                            let _ = state_inner.truncate_handle(h_id, 0);
                        } else {
                            state_inner.truncate_inode_handles(record.id, 0);
                        }
                    } else {
                        let blob = match pending.view_of(record.id) {
                            Some(view) => view.blob_id,
                            None => record.current_blob_id,
                        };
                        let mut orig = if let Some(blob_id) = blob {
                            let comp = store.get(blob_id, None).await?;
                            smartfs_compress::decompress(&comp).unwrap_or(comp)
                        } else {
                            Vec::new()
                        };
                        orig.resize(new_size as usize, 0);
                        state_inner.truncate_inode_handles_with_data(record.id, orig);
                    }
                } else {
                    let current_size = match pending.view_of(record.id) {
                        Some(view) => view.size as u64,
                        None => record.size as u64,
                    };

                    // Root Invariant #2: Standalone truncate (no open file descriptors) alters file content
                    // and must commit the truncated blob to pending pipeline.
                    if new_size != current_size || record.current_blob_id.is_none() {
                    let data = if new_size == 0 {
                        Vec::new()
                    } else {
                        let blob = match pending.view_of(record.id) {
                            Some(view) => view.blob_id,
                            None => record.current_blob_id,
                        };
                        let mut orig = if let Some(blob_id) = blob {
                            let comp = store.get(blob_id, None).await?;
                            smartfs_compress::decompress(&comp).unwrap_or(comp)
                        } else {
                            Vec::new()
                        };
                        orig.resize(new_size as usize, 0);
                        orig
                    };

                    let hash = smartfs_compress::hash_bytes(&data).0;
                    let new_blob_uuid = Uuid::new_v4();
                    let backend_id = record.backend_id;
                    let compression_level = record.compression_level as i32;

                    let insert_result = smartfs_db::insert_blob(
                        &pool,
                        &hash,
                        new_blob_uuid,
                        backend_id,
                        new_size as i64,
                    )
                    .await?;
                    let blob_id = insert_result.blob_id;

                    let mut compressed_size = None;
                    if insert_result.inserted {
                        let compressed =
                            match smartfs_compress::compress(&data, compression_level) {
                                Ok(c) => c,
                                Err(e) => {
                                    let _ = smartfs_db::compensate_blob_delete(&pool, &hash).await;
                                    return Err(e);
                                }
                            };

                        match store.put(blob_id, &compressed).await {
                            Ok(()) => {
                                let c_size = compressed.len() as i64;
                                compressed_size = Some(c_size);
                                let _ = smartfs_db::update_blob_compressed_size(
                                    &pool, &hash, c_size,
                                )
                                .await;
                            }
                            Err(e) => {
                                let _ = smartfs_db::compensate_blob_delete(&pool, &hash).await;
                                return Err(e);
                            }
                        }
                    } else {
                        if let Ok(exists) = store.exists(blob_id).await {
                            if !exists {
                                if let Ok(compressed) =
                                    smartfs_compress::compress(&data, compression_level)
                                {
                                    let _ = store.put(blob_id, &compressed).await;
                                }
                            }
                        }
                    }

                    let seq = pending.next_seq();
                    let marker = smartfs_schema::PendingMarker {
                        seq,
                        inode_id: record.id,
                        parent_inode: record.parent_id,
                        name: record.name.clone(),
                        version_id: Uuid::new_v4(),
                        content_hash: hash.clone(),
                        blob_id: Some(blob_id),
                        size: new_size as i64,
                        compressed_size,
                        external_path: None,
                        mode: mode.map(|m| m & 0o7777).unwrap_or(record.mode as u32),
                        uid: uid.unwrap_or(record.uid as u32),
                        gid: gid.unwrap_or(record.gid as u32),
                        special_type: Some("generic".to_string()),
                        special_data: None,
                        ast_nodes: serde_json::Value::Array(Vec::new()),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    };

                    pending.submit(&marker).await?;
                    truncated_data = Some(data);
                }
            }
        }

            // ADR-61: utimensat used to be a silent no-op — these arguments were
            // accepted and dropped, so the call returned 0 and nothing changed.
            // UTIME_OMIT arrives as None and must leave the field alone;
            // UTIME_NOW arrives as TimeOrNow::Now.
            let to_utc = |t: TimeOrNow| -> chrono::DateTime<chrono::Utc> {
                match t {
                    TimeOrNow::Now => chrono::Utc::now(),
                    TimeOrNow::SpecificTime(st) => chrono::DateTime::<chrono::Utc>::from(st),
                }
            };
            if atime.is_some() || mtime.is_some() {
                smartfs_db::inode_set_times(
                    &pool,
                    record.id,
                    atime.map(to_utc),
                    mtime.map(to_utc),
                )
                .await?;
            }

            let updated = smartfs_db::inode_update_attrs(
                &pool,
                record.id,
                uid.map(|u| u as i32),
                gid.map(|g| g as i32),
                // Keep S_IFMT: chmod changes permissions, never the type. Masking
                // the request outright used to store a mode with no type bits,
                // which after ADR-59 would turn the file into "type unknown".
                mode.map(|m| (((record.mode as u32) & libc::S_IFMT) | (m & 0o7777)) as i32),
                None, // size is managed exclusively by cow_commit / drain pipeline
            )
            .await?;

            Ok((updated, truncated_data))
        });

        match res {
            Ok((record, truncated_data)) => {
                if let Some(data) = truncated_data {
                    if let Some(h_id) = fh {
                        let _ = state.truncate_handle_committed(h_id, &data);
                    }
                    state.truncate_inode_handles_committed(record.id, &data);
                }
                let mut attr = inode_to_file_attr(&record);
                if let Some(new_size) = size {
                    attr.size = new_size;
                    attr.blocks = new_size.div_ceil(512);
                }
                reply.attr(&TTL, &attr);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let pool = self.pool.clone();
        let state = self.state.clone();

        let res: Result<(u64, Vec<smartfs_db::InodeRecord>)> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Directory inode {ino} not found"))
                })?;

            if !record.is_dir {
                return Err(SmartFsError::Other(format!(
                    "Inode {ino} is not a directory"
                )));
            }

            let parent_ino = match record.parent_id {
                Some(parent_uuid) if record.ino != 1 => {
                    let p = smartfs_db::inode_get(&pool, parent_uuid)
                        .await?
                        .ok_or_else(|| SmartFsError::NotFound("Parent inode not found".into()))?;
                    p.ino as u64
                }
                _ => 1u64,
            };

            let children = smartfs_db::inode_list_children(&pool, Some(record.id)).await?;
            let filtered: Vec<_> = children
                .into_iter()
                .filter(|c| !state.is_marked_for_deletion(c.id))
                .collect();

            Ok((parent_ino, filtered))
        });

        match res {
            Ok((parent_ino, children)) => {
                // Synthesize . and .. explicitly, stable ORDER BY ino
                let mut entries = Vec::with_capacity(children.len() + 2);
                entries.push((1i64, ino, FileType::Directory, ".".to_string()));
                entries.push((2i64, parent_ino, FileType::Directory, "..".to_string()));

                for (idx, child) in children.into_iter().enumerate() {
                    // One derivation for every path (ADR-59), so readdir cannot
                    // disagree with getattr about what a file is.
                    let kind = crate::state::file_type_of(&child);
                    entries.push(((idx + 3) as i64, child.ino as u64, kind, child.name));
                }

                for entry in entries.into_iter().skip(offset as usize) {
                    if reply.add(entry.1, entry.0, entry.2, &entry.3) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let state = self.state.clone();
        let uid = req.uid() as i32;
        let gid = req.gid() as i32;

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let parent_id = if parent == fuser::FUSE_ROOT_ID {
                let root = smartfs_db::inode_lookup_by_ino(&pool, 1)
                    .await?
                    .ok_or_else(|| SmartFsError::NotFound("Root inode not found".into()))?;
                state.cache_ino_mapping(1, root.id);
                root.id
            } else {
                match state.get_cached_id(parent) {
                    Some(id) => id,
                    None => {
                        let p = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                            .await?
                            .ok_or_else(|| {
                                SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                            })?;
                        state.cache_ino_mapping(parent, p.id);
                        p.id
                    }
                }
            };

            let created = smartfs_db::inode_create(
                &pool,
                Some(parent_id),
                &name_str,
                false,
                uid,
                gid,
                // ADR-59: store the full mode. create(2) is only ever called for
                // regular files, so the type is S_IFREG by definition.
                (libc::S_IFREG | (mode & 0o7777)) as i32,
            )
            .await?;

            state.cache_ino_mapping(created.ino as u64, created.id);
            Ok(created)
        });

        match res {
            Ok(created) => {
                let fh = self.state.allocate_fh(
                    created.ino as u64,
                    created.id,
                    flags,
                    Some(Vec::new()),
                );
                let attr = inode_to_file_attr(&created);
                reply.created(&TTL, &attr, 1, fh, 0);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let pool = self.pool.clone();
        let state = self.state.clone();

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

            if state.is_marked_for_deletion(record.id) {
                return Err(SmartFsError::NotFound(format!("Inode {ino} is unlinked")));
            }

            state.cache_ino_mapping(ino, record.id);
            Ok(record)
        });

        match res {
            Ok(record) => {
                let fh = self.state.allocate_fh(ino, record.id, flags, None);
                reply.opened(fh, 0);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let state = self.state.clone();

        // 1. Read from per-fd buffer if modified/present
        let mut buffer_data = None;
        if let Some(h) = state.get_handle(fh) {
            if let Some(buf) = h.buffer {
                buffer_data = Some(buf);
            }
        }

        let data = if let Some(buf) = buffer_data {
            buf
        } else {
            // 2. Otherwise load once into this handle's buffer.
            //
            // ADR-16 always specified a per-fd RAM buffer holding the whole
            // file; read simply never populated it, so every read() re-fetched
            // the blob from the store and decompressed all of it just to return
            // one slice. Reading a 64 MB file in 128 KB chunks therefore
            // decompressed 64 MB roughly five hundred times, which the first
            // perf run measured as 1.16 MB/s.
            let load = (|| -> Result<()> {
                if state.needs_buffer(fh)? {
                    let data = self.load_current_bytes(ino)?;
                    state.set_buffer_if_absent(fh, data)?;
                }
                Ok(())
            })();
            if let Err(e) = load {
                reply.error(error_to_errno(&e));
                return;
            }
            if let Some(inode_id) = self.state.get_handle(fh).map(|h| h.inode_id) {
                self.note_access(inode_id);
            }
            match self.state.get_handle(fh).and_then(|h| h.buffer) {
                Some(buf) => buf,
                // No handle: fall back to a direct load rather than failing a
                // read the caller is entitled to.
                None => match self.load_current_bytes(ino) {
                    Ok(loaded) => loaded,
                    Err(e) => {
                        reply.error(error_to_errno(&e));
                        return;
                    }
                },
            }
        };

        if offset < 0 || offset as usize >= data.len() {
            reply.data(&[]);
        } else {
            let start = offset as usize;
            let end = std::cmp::min(start + size as usize, data.len());
            reply.data(&data[start..end]);
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let state = self.state.clone();

        // Load outside the handles lock, then publish — see needs_buffer().
        let init_res = (|| -> Result<()> {
            if state.needs_buffer(fh)? {
                let data = self.load_current_bytes(ino)?;
                state.set_buffer_if_absent(fh, data)?;
            }
            Ok(())
        })();

        if let Err(e) = init_res {
            reply.error(error_to_errno(&e));
            return;
        }

        match self
            .state
            .write_to_handle(fh, offset.max(0) as usize, data)
        {
            Ok(bytes_written) => reply.written(bytes_written as u32),
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        let handle = match self.state.get_handle(fh) {
            Some(h) => h,
            None => {
                reply.ok();
                return;
            }
        };

        if handle.modified {
            if let Some(buf) = &handle.buffer {
                let pool = self.pool.clone();
                let filename_res = self.block_on(async move {
                    let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64).await?;
                    Ok::<_, SmartFsError>(record.map(|r| r.name).unwrap_or_default())
                });

                let filename = filename_res.unwrap_or_default();
                let code_str = String::from_utf8_lossy(buf).to_string();

                let ast_res = self.block_on(async move {
                    crate::syntax::validate_and_extract_ast_blocking(code_str, filename).await
                });

                match ast_res {
                    Ok(nodes) => {
                        self.state.store_ast_nodes(fh, nodes);
                    }
                    Err(SmartFsError::SyntaxError(err)) => {
                        if !self.force {
                            tracing::warn!("Syntax validation error on flush (EACCES): {err}");
                            reply.error(libc::EACCES);
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("AST extraction error: {e}");
                    }
                }
            }
        }

        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let closed = self.state.close_handle(fh);
        if let Some((handle, should_delete)) = closed {
            if handle.modified {
                if let Some(data) = handle.buffer {
                    let pool = self.pool.clone();
                    let store = self.store.clone();
                    let inode_id = handle.inode_id;
                    let ast_nodes = handle.ast_nodes;
                    let pending = self.pending.clone();
                    let seq = self.pending.next_seq();

                    let commit = self.block_on(async move {
                        // Phase timing for the write path. close() latency is the
                        // number ADR-58 set out to change and the one stage 5
                        // measures, and a single total tells you nothing about
                        // which of six steps owns it. Instant::now() costs a few
                        // nanoseconds against milliseconds of I/O, so this is
                        // always collected; only the emit is level-gated.
                        let t_start = std::time::Instant::now();
                        let record = smartfs_db::inode_get(&pool, inode_id).await?.ok_or_else(
                            || SmartFsError::NotFound(format!("Inode {inode_id} not found")),
                        )?;
                        let t_lookup = t_start.elapsed();

                        let hash = smartfs_compress::hash_bytes(&data).0;
                        let size = data.len() as i64;
                        let new_blob_uuid = Uuid::new_v4();
                        let backend_id = record.backend_id;
                        let compression_level = record.compression_level as i32;

                        // Step 1: Outside transaction
                        let t_hash = t_start.elapsed();
                        // ADR-62: the inode's policy decides whether this blob
                        // joins the dedup index. A private blob keeps its
                        // inventory row but stays out of the partial unique
                        // index, so nothing else can ever come to depend on it
                        // — which is what lets unlink free it immediately.
                        let insert_result = smartfs_db::insert_blob_with_sharing(
                            &pool,
                            &hash,
                            new_blob_uuid,
                            backend_id,
                            size,
                            record.dedup_enabled,
                        )
                        .await?;
                        let blob_id = insert_result.blob_id;
                        let t_dedup = t_start.elapsed();

                        let mut compressed_size = None;
                        if insert_result.inserted {
                            let compressed =
                                match smartfs_compress::compress(&data, compression_level) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        let _ = smartfs_db::compensate_blob_delete(&pool, &hash)
                                            .await;
                                        return Err(e);
                                    }
                                };

                            match store.put(blob_id, &compressed).await {
                                Ok(()) => {
                                    let c_size = compressed.len() as i64;
                                    compressed_size = Some(c_size);
                                    let _ = smartfs_db::update_blob_compressed_size(
                                        &pool, &hash, c_size,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    // FIX-04: Compensate delete on store.put failure
                                    let _ = smartfs_db::compensate_blob_delete(&pool, &hash)
                                        .await;
                                    return Err(e);
                                }
                            }
                        } else {
                            // FIX-03 healing: verify physical existence
                            if let Ok(exists) = store.exists(blob_id).await {
                                if !exists {
                                    if let Ok(compressed) =
                                        smartfs_compress::compress(&data, compression_level)
                                    {
                                        let _ = store.put(blob_id, &compressed).await;
                                    }
                                }
                            }
                        }

                        // Step 2 (ADR-58): no longer the Postgres transaction.
                        // Publish a durable pending marker and hand it to the
                        // drain. The blob above is already on disk, so once the
                        // marker's rename() lands the write survives a crash
                        // even though nothing has touched file_versions yet.
                        let marker = smartfs_schema::PendingMarker {
                            seq,
                            inode_id,
                            parent_inode: record.parent_id,
                            name: record.name.clone(),
                            // Idempotency key for replay: the drain inserts
                            // with ON CONFLICT (id) DO NOTHING, so a marker
                            // that survived its own COMMIT replays as a no-op.
                            version_id: Uuid::new_v4(),
                            content_hash: hash.clone(),
                            blob_id: Some(blob_id),
                            size,
                            compressed_size,
                            external_path: None,
                            mode: record.mode as u32,
                            uid: record.uid as u32,
                            gid: record.gid as u32,
                            special_type: Some("generic".to_string()),
                            special_data: None,
                            ast_nodes: serde_json::to_value(&ast_nodes).unwrap_or_else(
                                |_| serde_json::Value::Array(Vec::new()),
                            ),
                            created_at: chrono::Utc::now().to_rfc3339(),
                        };

                        let t_blob = t_start.elapsed();
                        pending.submit(&marker).await?;
                        let t_total = t_start.elapsed();

                        // One line, fixed field order, so stage 5 can aggregate
                        // it without parsing prose.
                        tracing::debug!(
                            target: "smartfs::write_phases",
                            "phases_us lookup={} hash={} dedup={} blob={} marker={} total={}",
                            t_lookup.as_micros(),
                            (t_hash - t_lookup).as_micros(),
                            (t_dedup - t_hash).as_micros(),
                            (t_blob - t_dedup).as_micros(),
                            (t_total - t_blob).as_micros(),
                            t_total.as_micros()
                        );

                        Ok::<_, SmartFsError>(())
                    });

                    // A refused write must reach the caller. Swallowing this is
                    // how an acknowledged-but-lost write happens, which is the
                    // one outcome ADR-58 exists to prevent.
                    if let Err(e) = commit {
                        tracing::error!("release() could not queue the write: {e}");
                        reply.error(error_to_errno(&e));
                        return;
                    }
                }
            }

            if should_delete {
                let pool = self.pool.clone();
                let inode_id = handle.inode_id;
                let _ = self.block_on(async move {
                    smartfs_db::inode_delete(&pool, inode_id).await
                });
            }
        }

        reply.ok();
    }

    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let pending = self.pending.clone();
        let _ = self.block_on(async move {
            pending.quiesce(Duration::from_secs(5)).await
        });
        reply.ok();
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _newparent: u64,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(libc::ENOTSUP);
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };
        let newname_str = match newname.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 || newname_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let store = self.store.clone();
        let state = self.state.clone();
        let force = self.force;

        let res: Result<()> = self.block_on(async move {
            let old_parent = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Old parent {parent} not found")))?;
            let new_parent = smartfs_db::inode_lookup_by_ino(&pool, newparent as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("New parent {newparent} not found")))?;

            // Check syntax validation on rename if destination matches plugin with ast=true (§3.6, §11.3)
            if !force && crate::syntax::detect_syntax_language(&newname_str).is_some() {
                if let Some(source_rec) =
                    smartfs_db::inode_lookup(&pool, Some(old_parent.id), &name_str).await?
                {
                    let content = if let Some(blob_id) = source_rec.current_blob_id {
                        let compressed = store.get(blob_id, None).await?;
                        smartfs_compress::decompress(&compressed).unwrap_or(compressed)
                    } else {
                        Vec::new()
                    };
                    let code_str = String::from_utf8_lossy(&content).to_string();
                    crate::syntax::validate_and_extract_ast_blocking(
                        code_str,
                        newname_str.clone(),
                    )
                    .await?;
                }
            }

            // Atomic rename: collision with existing target removed in same transaction
            smartfs_db::inode_rename(
                &pool,
                Some(old_parent.id),
                &name_str,
                Some(new_parent.id),
                &newname_str,
            )
            .await?;

            state.cache_ino_mapping(old_parent.ino as u64, old_parent.id);
            state.cache_ino_mapping(new_parent.ino as u64, new_parent.id);

            Ok(())
        });

        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let state = self.state.clone();
        let store = self.store.clone();

        let res: std::result::Result<(), libc::c_int> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await
                .map_err(|e| error_to_errno(&e))?
                .ok_or(libc::ENOENT)?;

            let child = smartfs_db::inode_lookup(&pool, Some(parent_record.id), &name_str)
                .await
                .map_err(|e| error_to_errno(&e))?
                .ok_or(libc::ENOENT)?;

            if child.is_dir {
                return Err(libc::EPERM);
            }

            // ADR-62 §Rozstrzygnięcia #3: for a private blob the space comes
            // back now, not eventually. A private blob has exactly one owner, so
            // no proof of non-reference is needed — and one unlink(2) is noise
            // beside the Postgres commit this path already pays for
            // inode_delete. A shared blob is left alone: freeing it needs the
            // scan, which is the cleaner's job.
            let private_blob = smartfs_db::private_blob_of_inode(&pool, child.id)
                .await
                .unwrap_or(None);

            let should_delete_now = state.mark_for_deletion(child.id);
            if should_delete_now {
                smartfs_db::inode_delete(&pool, child.id)
                    .await
                    .map_err(|e| error_to_errno(&e))?;

                if let Some(blob_id) = private_blob {
                    // Order matters: the row goes first, so a crash between the
                    // two leaves an orphan file — invisible through the mount
                    // and reclaimable — rather than a row pointing at bytes that
                    // are gone, which would fail every read of it forever.
                    if let Err(e) = store.delete(blob_id).await {
                        tracing::warn!("private blob {blob_id} left behind after unlink: {e}");
                    }
                }
            }

            Ok(())
        });

        match res {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let state = self.state.clone();

        let res: std::result::Result<(), libc::c_int> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await
                .map_err(|e| error_to_errno(&e))?
                .ok_or(libc::ENOENT)?;

            let child = smartfs_db::inode_lookup(&pool, Some(parent_record.id), &name_str)
                .await
                .map_err(|e| error_to_errno(&e))?
                .ok_or(libc::ENOENT)?;

            if !child.is_dir {
                return Err(libc::ENOTDIR);
            }

            let children = smartfs_db::inode_list_children(&pool, Some(child.id))
                .await
                .map_err(|e| error_to_errno(&e))?;

            let active_children: Vec<_> = children
                .into_iter()
                .filter(|c| !state.is_marked_for_deletion(c.id))
                .collect();

            if !active_children.is_empty() {
                return Err(libc::ENOTEMPTY);
            }

            let should_delete_now = state.mark_for_deletion(child.id);
            if should_delete_now {
                smartfs_db::inode_delete(&pool, child.id)
                    .await
                    .map_err(|e| error_to_errno(&e))?;
            }

            Ok(())
        });

        match res {
            Ok(()) => reply.ok(),
            Err(errno) => reply.error(errno),
        }
    }

    fn mkdir(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let pool = self.pool.clone();
        let state = self.state.clone();
        let uid = req.uid() as i32;
        let gid = req.gid() as i32;

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                })?;

            let dir = smartfs_db::inode_create(
                &pool,
                Some(parent_record.id),
                &name_str,
                true,
                uid,
                gid,
                // ADR-59: store the full mode, matching the root inode which
                // migration 001 already seeds as 0o40755.
                (libc::S_IFDIR | (mode & 0o7777)) as i32,
            )
            .await?;

            state.cache_ino_mapping(dir.ino as u64, dir.id);
            Ok(dir)
        });

        match res {
            Ok(dir) => {
                let attr = inode_to_file_attr(&dir);
                reply.entry(&TTL, &attr, 1);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn mknod(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        // ADR-59: every POSIX type is representable. FIFOs, sockets and device
        // nodes have no data path at all — the kernel implements their
        // semantics once getattr reports the type — so they cost one inode row
        // and nothing else. mknod(2) leaves the type unspecified as 0, which
        // POSIX treats as a regular file.
        let file_type = match mode & libc::S_IFMT {
            0 => libc::S_IFREG,
            t @ (libc::S_IFREG
            | libc::S_IFIFO
            | libc::S_IFSOCK
            | libc::S_IFCHR
            | libc::S_IFBLK) => t,
            // S_IFDIR belongs to mkdir and S_IFLNK to symlink; mknod(2) says
            // EINVAL for a type it does not create, not ENOTSUP.
            _ => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        // rdev is meaningful only for device nodes; mknod(2) ignores it otherwise.
        let node_rdev = if file_type == libc::S_IFCHR || file_type == libc::S_IFBLK {
            rdev as i64
        } else {
            0
        };
        let stored_mode = (file_type | (mode & 0o7777)) as i32;

        let pool = self.pool.clone();
        let state = self.state.clone();
        let uid = req.uid() as i32;
        let gid = req.gid() as i32;

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                })?;

            let file = smartfs_db::inode_create_with_rdev(
                &pool,
                Some(parent_record.id),
                &name_str,
                false,
                uid,
                gid,
                stored_mode,
                node_rdev,
            )
            .await?;

            state.cache_ino_mapping(file.ino as u64, file.id);
            Ok(file)
        });

        match res {
            Ok(file) => {
                let attr = inode_to_file_attr(&file);
                reply.entry(&TTL, &attr, 1);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn symlink(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let name_str = match link_name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

        if name_str.len() > 255 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let target_bytes = target.as_os_str().as_bytes().to_vec();
        if target_bytes.is_empty() {
            reply.error(libc::ENOENT);
            return;
        }
        if target_bytes.len() > 4096 {
            reply.error(libc::ENAMETOOLONG);
            return;
        }

        let target_len = target_bytes.len();
        let pool = self.pool.clone();
        let store = self.store.clone();
        let state = self.state.clone();
        let pending = self.pending.clone();
        let uid = req.uid() as i32;
        let gid = req.gid() as i32;

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                })?;

            let hash = smartfs_compress::hash_bytes(&target_bytes).0;
            let blob_uuid = Uuid::new_v4();
            let size = target_bytes.len() as i64;

            let insert_result =
                smartfs_db::insert_blob(&pool, &hash, blob_uuid, None, size).await?;
            let blob_id = insert_result.blob_id;

            let mut compressed_size = None;
            if insert_result.inserted {
                let compressed = match smartfs_compress::compress(&target_bytes, 1) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = smartfs_db::compensate_blob_delete(&pool, &hash).await;
                        return Err(e);
                    }
                };
                match store.put(blob_id, &compressed).await {
                    Ok(()) => {
                        let c_size = compressed.len() as i64;
                        compressed_size = Some(c_size);
                        let _ =
                            smartfs_db::update_blob_compressed_size(&pool, &hash, c_size).await;
                    }
                    Err(e) => {
                        let _ = smartfs_db::compensate_blob_delete(&pool, &hash).await;
                        return Err(e);
                    }
                }
            } else if let Ok(exists) = store.exists(blob_id).await {
                if !exists {
                    if let Ok(compressed) = smartfs_compress::compress(&target_bytes, 1) {
                        let _ = store.put(blob_id, &compressed).await;
                    }
                }
            }

            let mode = (libc::S_IFLNK | 0o777) as i32;
            let created = smartfs_db::inode_create(
                &pool,
                Some(parent_record.id),
                &name_str,
                false,
                uid,
                gid,
                mode,
            )
            .await?;

            let seq = pending.next_seq();
            let marker = smartfs_schema::PendingMarker {
                seq,
                inode_id: created.id,
                parent_inode: created.parent_id,
                name: created.name.clone(),
                version_id: Uuid::new_v4(),
                content_hash: hash.clone(),
                blob_id: Some(blob_id),
                size,
                compressed_size,
                external_path: None,
                mode: created.mode as u32,
                uid: created.uid as u32,
                gid: created.gid as u32,
                special_type: Some("symlink".to_string()),
                special_data: None,
                ast_nodes: serde_json::Value::Array(Vec::new()),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            pending.submit(&marker).await?;

            state.cache_ino_mapping(created.ino as u64, created.id);
            Ok(created)
        });

        match res {
            Ok(created) => {
                let mut attr = inode_to_file_attr(&created);
                attr.kind = FileType::Symlink;
                attr.size = target_len as u64;
                attr.blocks = attr.size.div_ceil(512);
                reply.entry(&TTL, &attr, 1);
            }
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        let pool = self.pool.clone();
        let store = self.store.clone();
        let pending = self.pending.clone();

        let res: Result<Vec<u8>> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

            if (record.mode as u32 & libc::S_IFMT) != libc::S_IFLNK {
                return Err(SmartFsError::Io(std::io::Error::from_raw_os_error(libc::EINVAL)));
            }

            let blob = match pending.view_of(record.id) {
                Some(view) => view.blob_id,
                None => record.current_blob_id,
            };

            if let Some(blob_id) = blob {
                let compressed = store.get(blob_id, None).await?;
                let decompressed = smartfs_compress::decompress(&compressed).unwrap_or(compressed);
                Ok(decompressed)
            } else {
                Ok(Vec::new())
            }
        });

        match res {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let stat = get_statvfs(&self.blob_dir);
        if let Some(s) = stat {
            reply.statfs(
                s.f_blocks,
                s.f_bfree,
                s.f_bavail,
                s.f_files,
                s.f_ffree,
                s.f_bsize as u32,
                s.f_namemax as u32,
                s.f_frsize as u32,
            );
        } else {
            // Sensible fallback if statvfs fails on the host path (§3.6: never ENOSYS)
            reply.statfs(
                1_000_000, 500_000, 500_000, 100_000, 50_000, 4096, 255, 4096,
            );
        }
    }
}

fn get_statvfs(path: &Path) -> Option<libc::statvfs> {
    let c_path = CString::new(path.to_str()?).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    let res = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if res == 0 {
        Some(unsafe { stat.assume_init() })
    } else {
        None
    }
}
