//! The `pending` marker — the durable record of a write that has been
//! acknowledged to the caller but not yet committed to Postgres (ADR-58).
//!
//! Lives here rather than in `smartfs-store` or `smartfs-db` because both sides
//! need it: `smartfs-fuse` writes it in the pending stage, and the drain reads
//! it back to build exactly the transaction `cow_commit` used to run inline.
//! Consistent with this crate's contract, it is a type and nothing else — zero
//! logic, zero I/O, zero SQL.
//!
//! Root Invariant #1 note: `content_hash` here is the SHA-256 of the file's
//! plaintext bytes, computed before compression. The marker and its metadata
//! never enter that hash (ADR-58, hard rule 2).

use serde::{Deserialize, Serialize};

use crate::types::Uuid;

/// @id: 603f3b03-1233-4f37-872f-8077039c286f
/// One queued write, as persisted at `<store>/pending/queue/<seq>_<hash>.json`.
///
/// The file it identifies is addressed by `inode_id`; `parent_inode` and `name`
/// are carried for diagnostics and for the CLI's repair reporting. The absolute
/// path is deliberately **not** stored: it is derivable, it is never a key
/// (ADR-58 §Rozstrzygnięcia #3 made `version_id` the key), and resolving it
/// would put a parent-walk back onto the very hot path this ADR exists to
/// shorten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingMarker {
    /// Monotonic per-daemon sequence. Also encoded in the filename, so the
    /// drain can order the queue from `readdir` alone.
    pub seq: u64,

    /// `inode_registry.id` of the file this version belongs to.
    pub inode_id: Uuid,

    /// Parent directory's `inode_registry.id`, `None` for the root.
    pub parent_inode: Option<Uuid>,

    /// File name within its parent directory. Diagnostics only, never a key.
    pub name: String,

    /// Primary key the drain will insert this version under, generated here in
    /// the pending stage. This is the replay idempotency key (ADR-58
    /// §Rozstrzygnięcia #3, rewizja): the drain inserts with
    /// `ON CONFLICT (id) DO NOTHING`, so replaying a marker whose transaction
    /// already committed is a no-op.
    ///
    /// `version_number` is deliberately NOT carried: it is computed as `MAX+1`
    /// inside the drain transaction, because a number allocated here would not
    /// be visible to `smartfs-cli` writing to the same database, and two queued
    /// writes to one file would collide on it.
    pub version_id: Uuid,

    /// SHA-256 of the plaintext bytes, before compression (Root Invariant #1).
    pub content_hash: String,

    /// Blob this version points at, `None` for an external-path version.
    pub blob_id: Option<Uuid>,

    /// Plaintext byte length.
    pub size: i64,

    /// Compressed byte length, `None` when the blob was already deduplicated
    /// or the compressed size was not recorded before the crash.
    pub compressed_size: Option<i64>,

    /// Set instead of `blob_id` for content that lives outside the blob store.
    pub external_path: Option<String>,

    /// POSIX mode of the file at the time of the write.
    pub mode: u32,

    /// Owning uid at the time of the write.
    pub uid: u32,

    /// Owning gid at the time of the write.
    pub gid: u32,

    /// Plugin type driving `special_data`, `None` for `generic`.
    pub special_type: Option<String>,

    /// Plugin payload, passed through to `file_versions.special_data`.
    pub special_data: Option<serde_json::Value>,

    /// AST nodes extracted at flush time, carried as opaque JSON so this crate
    /// stays free of `smartfs-db`'s row types. Deserialized by the drain into
    /// `Vec<AstNodeInsert>`.
    pub ast_nodes: serde_json::Value,

    /// RFC 3339 timestamp of the pending stage, for queue-age reporting.
    pub created_at: String,
}

/// @id: 569d7df3-825f-4600-972b-8080e71fb3ac
impl PendingMarker {
    /// @id: 71a12f00-a067-4ddd-aae2-9044bb26f0dd
    /// Filename this marker is stored under: `<seq>_<content_hash>.json`.
    ///
    /// `seq` is zero-padded to 20 digits (the width of `u64::MAX`) so a plain
    /// lexicographic sort of `readdir` output is FIFO order.
    pub fn file_name(&self) -> String {
        Self::file_name_for(self.seq, &self.content_hash)
    }

    /// @id: 4c65e76c-91f4-45ca-bd3f-db726d688a7b
    /// Builds a marker filename without needing the marker itself.
    pub fn file_name_for(seq: u64, content_hash: &str) -> String {
        format!("{seq:020}_{content_hash}.json")
    }

    /// @id: 9775cc48-4efb-4771-86d8-8238f9cf5cc1
    /// Parses `<seq>_<content_hash>.json` back into its two parts.
    ///
    /// Returns `None` for anything that is not a well-formed marker name, so a
    /// stray file in the queue directory is skipped rather than misread.
    pub fn parse_file_name(name: &str) -> Option<(u64, &str)> {
        let stem = name.strip_suffix(".json")?;
        let (seq, hash) = stem.split_once('_')?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some((seq.parse().ok()?, hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(seq: u64, hash: &str) -> PendingMarker {
        PendingMarker {
            seq,
            inode_id: Uuid::new_v4(),
            parent_inode: None,
            name: "f.txt".to_string(),
            version_id: Uuid::new_v4(),
            content_hash: hash.to_string(),
            blob_id: Some(Uuid::new_v4()),
            size: 3,
            compressed_size: Some(3),
            external_path: None,
            mode: 0o100644,
            uid: 0,
            gid: 0,
            special_type: None,
            special_data: None,
            ast_nodes: serde_json::Value::Array(vec![]),
            created_at: "2026-09-13T00:00:00Z".to_string(),
        }
    }

    const H: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn file_name_round_trips() {
        let m = marker(42, H);
        let name = m.file_name();
        assert_eq!(name, format!("00000000000000000042_{H}.json"));
        assert_eq!(PendingMarker::parse_file_name(&name), Some((42, H)));
    }

    #[test]
    fn zero_padding_makes_lexicographic_order_fifo() {
        let mut names = [
            marker(10, H).file_name(),
            marker(2, H).file_name(),
            marker(1, H).file_name(),
        ];
        names.sort();
        let seqs: Vec<u64> = names
            .iter()
            .map(|n| PendingMarker::parse_file_name(n).unwrap().0)
            .collect();
        assert_eq!(seqs, vec![1, 2, 10]);
    }

    #[test]
    fn u64_max_still_fits_the_padding() {
        let name = PendingMarker::file_name_for(u64::MAX, H);
        assert_eq!(
            PendingMarker::parse_file_name(&name),
            Some((u64::MAX, H)),
            "20 digits must hold u64::MAX without truncating"
        );
    }

    #[test]
    fn malformed_names_are_rejected_not_guessed() {
        for bad in [
            "no-extension",
            "notanumber_.json",
            "1_short.json",
            "1_ZZZ0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.json",
            "1.json",
            ".json",
        ] {
            assert!(
                PendingMarker::parse_file_name(bad).is_none(),
                "should have rejected {bad:?}"
            );
        }
    }

    #[test]
    fn marker_survives_a_json_round_trip() {
        let m = marker(7, H);
        let text = serde_json::to_string(&m).unwrap();
        let back: PendingMarker = serde_json::from_str(&text).unwrap();
        assert_eq!(m, back);
    }
}
