use async_trait::async_trait;
use smartfs_schema::{Result, Uuid};
use tokio::io::AsyncRead;

/// Universal abstraction for physical blob storage backends.
/// Receives already-compressed bytes. Zero awareness of SQL, inodes, or compression.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Stores the provided byte slice under the given UUID.
    async fn put(&self, uuid: Uuid, data: &[u8]) -> Result<()>;

    /// Retrieves the content of a blob by UUID, or directly from `external_path` if specified.
    async fn get(&self, uuid: Uuid, external_path: Option<&str>) -> Result<Vec<u8>>;

    /// Streams bytes into storage under the given UUID.
    async fn put_stream(
        &self,
        uuid: Uuid,
        stream: Box<dyn AsyncRead + Send + Unpin>,
        size_hint: Option<u64>,
    ) -> Result<()>;

    /// Returns an asynchronous reader for a blob by UUID or external path.
    async fn get_stream(
        &self,
        uuid: Uuid,
        external_path: Option<&str>,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>>;

    /// Deletes a physical blob from the storage backend.
    async fn delete(&self, uuid: Uuid) -> Result<()>;

    /// Checks if a blob physically exists in the storage backend.
    async fn exists(&self, uuid: Uuid) -> Result<bool>;
}
