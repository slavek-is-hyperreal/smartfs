use std::path::{Path, PathBuf};
use async_trait::async_trait;
use smartfs_schema::{Result, SmartFsError, Uuid};
use tokio::fs::{self, File};
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::traits::BlobStore;

/// Local filesystem implementation of `BlobStore`.
/// Stores blobs in a flat directory at `{root}/{uuid}`.
/// Writes atomically via temporary write-then-rename on the same filesystem.
#[derive(Debug, Clone)]
pub struct LocalDiskStore {
    root: PathBuf,
}

impl LocalDiskStore {
    /// Creates a new `LocalDiskStore` targeting the given root directory.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Returns the target filesystem path for a blob UUID.
    pub fn blob_path(&self, uuid: Uuid) -> PathBuf {
        self.root.join(uuid.to_string())
    }

    /// Returns a temporary path used for atomic write-then-rename.
    fn temp_path(&self, uuid: Uuid) -> PathBuf {
        self.root.join(format!(".{}.tmp", uuid))
    }
}

#[async_trait]
impl BlobStore for LocalDiskStore {
    async fn put(&self, uuid: Uuid, data: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.root).await?;
        let temp_path = self.temp_path(uuid);
        let final_path = self.blob_path(uuid);

        {
            let mut file = File::create(&temp_path).await?;
            file.write_all(data).await?;
            file.flush().await?;
            file.sync_all().await?;
        }

        fs::rename(&temp_path, &final_path).await?;
        Ok(())
    }

    async fn get(&self, uuid: Uuid, external_path: Option<&str>) -> Result<Vec<u8>> {
        let path = match external_path {
            Some(p) => PathBuf::from(p),
            None => self.blob_path(uuid),
        };

        if !fs::try_exists(&path).await.unwrap_or(false) {
            return Err(SmartFsError::NotFound(format!("Blob not found at {:?}", path)));
        }

        let bytes = fs::read(&path).await?;
        Ok(bytes)
    }

    async fn put_stream(
        &self,
        uuid: Uuid,
        mut stream: Box<dyn AsyncRead + Send + Unpin>,
        _size_hint: Option<u64>,
    ) -> Result<()> {
        fs::create_dir_all(&self.root).await?;
        let temp_path = self.temp_path(uuid);
        let final_path = self.blob_path(uuid);

        {
            let mut file = File::create(&temp_path).await?;
            tokio::io::copy(&mut stream, &mut file).await?;
            file.flush().await?;
            file.sync_all().await?;
        }

        fs::rename(&temp_path, &final_path).await?;
        Ok(())
    }

    async fn get_stream(
        &self,
        uuid: Uuid,
        external_path: Option<&str>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>> {
        let path = match external_path {
            Some(p) => PathBuf::from(p),
            None => self.blob_path(uuid),
        };

        let file = File::open(&path).await?;
        Ok(Box::new(file))
    }

    async fn delete(&self, uuid: Uuid) -> Result<()> {
        let path = self.blob_path(uuid);
        if fs::try_exists(&path).await.unwrap_or(false) {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn exists(&self, uuid: Uuid) -> Result<bool> {
        let path = self.blob_path(uuid);
        Ok(fs::try_exists(&path).await.unwrap_or(false))
    }
}
