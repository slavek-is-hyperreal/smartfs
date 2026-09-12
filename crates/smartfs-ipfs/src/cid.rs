use smartfs_schema::{ContentHash, Result, SmartFsError};

/// Maximum file size for inline CIDv1 computation (256 KB, ADR-17).
/// Files larger than this require UnixFS DAG-PB chunking (post-MVP).
pub const MAX_INLINE_CID_SIZE: u64 = 256 * 1024;

const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Encodes binary data into base32 unpadded lowercase string with 'b' multibase prefix.
fn encode_base32_multibase(input: &[u8]) -> String {
    let mut result = String::with_capacity(1 + (input.len() * 8).div_ceil(5));
    result.push('b'); // Multibase base32 prefix

    let mut buffer = 0u64;
    let mut bits = 0;

    for &byte in input {
        buffer = (buffer << 8) | (byte as u64);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0x1f) as usize;
            result.push(BASE32_ALPHABET[index] as char);
        }
    }

    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        result.push(BASE32_ALPHABET[index] as char);
    }

    result
}

/// Computes an IPFS CIDv1 string from a 32-byte SHA-256 digest if file size < 256 KB.
/// Returns `None` if size >= 256 KB (per ADR-17).
pub fn compute_cid_from_digest(digest: &[u8; 32], size: u64) -> Option<String> {
    if size >= MAX_INLINE_CID_SIZE {
        return None;
    }

    // CIDv1 binary format for raw codec + sha2-256 multihash:
    // 0x01 (CIDv1) + 0x55 (raw codec) + 0x12 (sha2-256) + 0x20 (32 bytes length) + digest[32]
    let mut raw_cid = Vec::with_capacity(4 + 32);
    raw_cid.push(0x01); // CIDv1
    raw_cid.push(0x55); // raw codec
    raw_cid.push(0x12); // sha2-256 code
    raw_cid.push(0x20); // 32 bytes digest length
    raw_cid.extend_from_slice(digest);

    Some(encode_base32_multibase(&raw_cid))
}

/// Computes an IPFS CIDv1 string from `ContentHash` (hex-encoded SHA-256) and file size.
pub fn compute_cid(content_hash: &ContentHash, size: u64) -> Result<Option<String>> {
    if size >= MAX_INLINE_CID_SIZE {
        return Ok(None);
    }

    let bytes = hex::decode(&content_hash.0)
        .map_err(|e| SmartFsError::Other(format!("invalid hex in content hash: {e}")))?;

    if bytes.len() != 32 {
        return Err(SmartFsError::Other(format!(
            "content hash is not 32 bytes (got {})",
            bytes.len()
        )));
    }

    let mut digest = [0u8; 32];
    digest.copy_from_slice(&bytes);

    Ok(compute_cid_from_digest(&digest, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cid_computation_small_file() {
        // Known SHA-256 of empty string: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let empty_hash = ContentHash("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());
        let cid = compute_cid(&empty_hash, 0).unwrap();
        assert!(cid.is_some());
        let cid_str = cid.unwrap();
        // Canonical CIDv1 raw-sha256 for empty file starts with bafkrei
        assert!(cid_str.starts_with("bafkrei"));
        assert_eq!(cid_str, "bafkreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku");
    }

    #[test]
    fn test_cid_size_threshold() {
        let hash = ContentHash("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());

        // Under 256KB -> returns Some
        assert!(compute_cid(&hash, 256 * 1024 - 1).unwrap().is_some());

        // Exactly 256KB or larger -> returns None (ADR-17)
        assert!(compute_cid(&hash, 256 * 1024).unwrap().is_none());
        assert!(compute_cid(&hash, 1024 * 1024).unwrap().is_none());
    }
}
