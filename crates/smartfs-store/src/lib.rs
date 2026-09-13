//! smartfs-store — BlobStore trait and backend implementations for SmartFS.
//!
//! Receives already-compressed bytes. Zero database, hashing, or FUSE awareness.

pub mod local;
pub mod pending;
pub mod traits;

pub use local::LocalDiskStore;
pub use pending::PendingQueue;
pub use traits::BlobStore;

#[cfg(test)]
mod tests {
    use super::*;
    use smartfs_schema::Uuid;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_local_disk_store_crud() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();
        let payload = b"hello compressed smartfs blob";

        assert!(!store.exists(id).await.unwrap());

        store.put(id, payload).await.unwrap();
        assert!(store.exists(id).await.unwrap());

        let retrieved = store.get(id, None).await.unwrap();
        assert_eq!(retrieved, payload);

        store.delete(id).await.unwrap();
        assert!(!store.exists(id).await.unwrap());
    }

    #[tokio::test]
    async fn test_local_disk_store_streaming() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();
        let payload = b"streaming blob payload data here";

        let reader = Box::new(Cursor::new(payload.to_vec()));
        store.put_stream(id, reader, Some(payload.len() as u64)).await.unwrap();
        assert!(store.exists(id).await.unwrap());

        let mut read_stream = store.get_stream(id, None).await.unwrap();
        let mut buffer = Vec::new();
        tokio::io::copy(&mut read_stream, &mut buffer).await.unwrap();
        assert_eq!(buffer, payload);
    }

    #[tokio::test]
    async fn test_local_disk_store_external_path() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();

        let ext_file = dir.path().join("external_file.txt");
        tokio::fs::write(&ext_file, b"overlay external content").await.unwrap();

        let ext_path_str = ext_file.to_str().unwrap();
        let retrieved = store.get(id, Some(ext_path_str)).await.unwrap();
        assert_eq!(retrieved, b"overlay external content");
    }
}
