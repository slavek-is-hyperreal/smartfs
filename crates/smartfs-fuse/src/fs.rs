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
    ) -> Self {
        Self {
            pool,
            store,
            blob_dir: blob_dir.as_ref().to_path_buf(),
            rt_handle,
            state: Arc::new(FuseStateManager::new()),
            force,
        }
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
                let attr = inode_to_file_attr(&record);
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
                if let Some(h_id) = fh {
                    if let Some(handle) = self.state.get_handle(h_id) {
                        if let Some(buf) = &handle.buffer {
                            attr.size = buf.len() as u64;
                            attr.blocks = attr.size.div_ceil(512);
                        }
                    }
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
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let pool = self.pool.clone();
        let state = self.state.clone();

        // O_TRUNC / truncate buffer management in memory (§11.2a)
        if let Some(new_size) = size {
            if let Some(h_id) = fh {
                let _ = state.truncate_handle(h_id, new_size as usize);
            }
        }

        let res: Result<smartfs_db::InodeRecord> = self.block_on(async move {
            let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

            let updated = smartfs_db::inode_update_attrs(
                &pool,
                record.id,
                uid.map(|u| u as i32),
                gid.map(|g| g as i32),
                mode.map(|m| (m & 0o7777) as i32),
                size.map(|s| s as i64),
            )
            .await?;

            Ok(updated)
        });

        match res {
            Ok(record) => {
                let mut attr = inode_to_file_attr(&record);
                if let Some(h_id) = fh {
                    if let Some(handle) = self.state.get_handle(h_id) {
                        if let Some(buf) = &handle.buffer {
                            attr.size = buf.len() as u64;
                            attr.blocks = attr.size.div_ceil(512);
                        }
                    }
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
                    let kind = if child.is_dir {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    };
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
                (mode & 0o7777) as i32,
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
                reply.created(&TTL, &attr, 1, fh, flags as u32);
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
                reply.opened(fh, flags as u32);
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
        let pool = self.pool.clone();
        let store = self.store.clone();
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
            // 2. Otherwise read from store
            let res: Result<Vec<u8>> = self.block_on(async move {
                let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                    .await?
                    .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

                if let Some(blob_id) = record.current_blob_id {
                    let compressed = store.get(blob_id, None).await?;
                    let decompressed =
                        smartfs_compress::decompress(&compressed).unwrap_or(compressed);
                    Ok(decompressed)
                } else {
                    Ok(Vec::new())
                }
            });

            match res {
                Ok(loaded) => loaded,
                Err(e) => {
                    reply.error(error_to_errno(&e));
                    return;
                }
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
        let pool = self.pool.clone();
        let store = self.store.clone();
        let state = self.state.clone();

        // Ensure buffer initialized from store before mutating
        let init_res = state.ensure_buffer(fh, || {
            self.block_on(async move {
                let record = smartfs_db::inode_lookup_by_ino(&pool, ino as i64)
                    .await?
                    .ok_or_else(|| SmartFsError::NotFound(format!("Inode {ino} not found")))?;

                if let Some(blob_id) = record.current_blob_id {
                    let compressed = store.get(blob_id, None).await?;
                    let decompressed =
                        smartfs_compress::decompress(&compressed).unwrap_or(compressed);
                    Ok(decompressed)
                } else {
                    Ok(Vec::new())
                }
            })
        });

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

                    let _ = self.block_on(async move {
                        let record = smartfs_db::inode_get(&pool, inode_id).await?.ok_or_else(
                            || SmartFsError::NotFound(format!("Inode {inode_id} not found")),
                        )?;

                        let hash = smartfs_compress::hash_bytes(&data).0;
                        let size = data.len() as i64;
                        let new_blob_uuid = Uuid::new_v4();
                        let backend_id = record.backend_id;
                        let compression_level = record.compression_level as i32;

                        // Step 1: Outside transaction
                        let insert_result = smartfs_db::insert_blob(
                            &pool,
                            &hash,
                            new_blob_uuid,
                            backend_id,
                            size,
                        )
                        .await?;
                        let blob_id = insert_result.blob_id;

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

                        // Step 2: cow_commit
                        smartfs_db::cow_commit(
                            &pool,
                            inode_id,
                            Some(blob_id),
                            &hash,
                            size,
                            compressed_size,
                            None,
                            Some("generic"),
                            None,
                            &ast_nodes,
                        )
                        .await?;

                        Ok::<_, SmartFsError>(())
                    });
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

        let pool = self.pool.clone();
        let state = self.state.clone();

        let res: Result<()> = self.block_on(async move {
            let parent_record = smartfs_db::inode_lookup_by_ino(&pool, parent as i64)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Parent inode {parent} not found"))
                })?;

            let child = smartfs_db::inode_lookup(&pool, Some(parent_record.id), &name_str)
                .await?
                .ok_or_else(|| SmartFsError::NotFound(format!("Child '{name_str}' not found")))?;

            let should_delete_now = state.mark_for_deletion(child.id);
            if should_delete_now {
                smartfs_db::inode_delete(&pool, child.id).await?;
            }

            Ok(())
        });

        match res {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(error_to_errno(&e)),
        }
    }

    fn rmdir(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.unlink(req, parent, name, reply);
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
                (mode & 0o7777) as i32,
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
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };

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

            let file = smartfs_db::inode_create(
                &pool,
                Some(parent_record.id),
                &name_str,
                false,
                uid,
                gid,
                (mode & 0o7777) as i32,
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
