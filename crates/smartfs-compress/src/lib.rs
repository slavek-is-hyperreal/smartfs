//! smartfs-compress — Hash + Compress Pipeline.
//!
//! Enforces Root Invariant #1:
//! `content_hash = SHA-256(original bytes BEFORE compression)` — never after.

use std::io::Write;
use sha2::{Digest, Sha256};
use smartfs_schema::{ContentHash, Result, SmartFsError, Uuid};
use smartfs_store::BlobStore;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Default chunk size for streaming I/O pipeline (4 MB).
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// @id: 4f398a07-7ac9-476b-a103-1bffae5c2915
/// Computes SHA-256 hex string from raw, uncompressed bytes.
pub fn hash_bytes(data: &[u8]) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(data);
    ContentHash(hex::encode(hasher.finalize()))
}

/// @id: 857d5a34-f18d-40d3-ba04-8a6fed34bbcd
/// Compresses a slice of bytes using zstd at the specified compression level.
pub fn compress(data: &[u8], level: i32) -> Result<Vec<u8>> {
    zstd::encode_all(data, level)
        .map_err(|e| SmartFsError::Compression(format!("zstd compress error: {e}")))
}

/// @id: ae494a15-a6e8-4f91-84a9-ab0a803837d0
/// Decompresses zstd-compressed bytes back to original content.
pub fn decompress(compressed: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(compressed)
        .map_err(|e| SmartFsError::Compression(format!("zstd decompress error: {e}")))
}

/// @id: 3ce287aa-e9ba-441e-9e22-0ab750dbcc4e
/// Streams raw bytes from `source`, incrementally computing SHA-256 and zstd-compressing.
///
/// Execution order (Root Invariant #1):
/// 1. Feed each chunk to `Sha256::update()` (hash on ORIGINAL bytes)
/// 2. Feed each chunk to `zstd::Encoder` (compress after hashing)
/// 3. Call `store.put(uuid, &compressed)`
pub async fn write_blob_streaming<R: AsyncRead + Send + Unpin>(
    mut source: R,
    level: i32,
    store: &dyn BlobStore,
) -> Result<(Uuid, ContentHash, u64, u64)> {
    let uuid = Uuid::new_v4();
    let mut hasher = Sha256::new();
    let mut encoder = zstd::Encoder::new(Vec::new(), level)
        .map_err(|e| SmartFsError::Compression(format!("failed to init zstd encoder: {e}")))?;

    let mut chunk = vec![0u8; CHUNK_SIZE];
    let mut orig_size = 0u64;

    loop {
        let n = source.read(&mut chunk).await?;
        if n == 0 {
            break;
        }

        let slice = &chunk[..n];
        // 1. Hash raw original bytes
        hasher.update(slice);
        orig_size += n as u64;

        // 2. Stream into zstd encoder
        encoder.write_all(slice)
            .map_err(|e| SmartFsError::Compression(format!("zstd write error: {e}")))?;
    }

    let compressed = encoder.finish()
        .map_err(|e| SmartFsError::Compression(format!("zstd finish error: {e}")))?;
    let compressed_size = compressed.len() as u64;

    let content_hash = ContentHash(hex::encode(hasher.finalize()));

    // 3. Store compressed bytes
    store.put(uuid, &compressed).await?;

    Ok((uuid, content_hash, orig_size, compressed_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartfs_store::LocalDiskStore;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_hash_before_compress_pipeline() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let raw_data = b"The quick brown fox jumps over the lazy dog. Repeat repeat repeat 1234567890!";

        // Expected hash of raw_data
        let expected_hash = hash_bytes(raw_data);

        let (uuid, content_hash, orig_size, compressed_size) =
            write_blob_streaming(Cursor::new(raw_data), 3, &store).await.unwrap();

        assert_eq!(content_hash, expected_hash);
        assert_eq!(orig_size, raw_data.len() as u64);
        assert!(compressed_size > 0);

        // Verify stored bytes are compressed and can be decompressed back to raw_data
        let stored_bytes = store.get(uuid, None).await.unwrap();
        assert_eq!(stored_bytes.len(), compressed_size as usize);

        let decompressed = decompress(&stored_bytes).unwrap();
        assert_eq!(decompressed, raw_data);
    }

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original = b"SmartFS content-addressed storage layer roundtrip test.";
        let compressed = compress(original, 3).unwrap();
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }
}
