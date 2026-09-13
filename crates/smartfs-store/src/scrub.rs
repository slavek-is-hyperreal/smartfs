//! Checksum scrub for already-committed blobs (ADR-58 decision point 7).
//!
//! **This is not the pending scan.** They look adjacent and are opposites:
//!
//! | | looks for | finds |
//! |---|---|---|
//! | `pending::PendingQueue` scan | a database row missing for a file that exists | an uncommitted write |
//! | this scrub | a file gone bad for a row that exists | bit rot |
//!
//! The ADR is explicit that the two must not be confused in the implementation
//! or in the logs, so every message here says `scrub`, never `scan`.
//!
//! **Why the codec is injected.** Verifying a blob means decompressing it and
//! re-hashing the plaintext, but `smartfs-compress` already depends on this
//! crate — depending back on it would be a cycle. So the caller passes a
//! closure that turns stored bytes into the plaintext digest, and this module
//! keeps only what it owns: walking the store, sampling, rate-limiting and
//! reporting. It stays free of SQL too; expected digests arrive as data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use smartfs_schema::{Result, Uuid};
use tokio::fs;

use crate::local::LocalDiskStore;

/// @id: 11238478-5d2e-4bb2-bbf7-61070094d1f4
/// One blob that failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubFinding {
    /// Blob whose stored bytes no longer match the recorded hash.
    pub blob_id: Uuid,
    /// `content_hash` the database records for it.
    pub expected: String,
    /// What the bytes on disk actually hash to, or the decode error.
    pub actual: ScrubActual,
    /// Where the file lives, so an operator can act on it.
    pub path: PathBuf,
}

/// @id: 98d1b13c-6604-4203-894d-024bb3427f2a
/// What a failing blob turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrubActual {
    /// Decoded cleanly, but to different content. Silent corruption.
    Digest(String),
    /// Could not even be decompressed. Structural damage.
    Undecodable(String),
    /// The database references it and the file is not there.
    Missing,
}

/// @id: ddd52374-0efd-4087-ab21-687a5784f512
/// Result of one scrub pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubReport {
    /// Blobs whose bytes were read and verified.
    pub checked: usize,
    /// Of those, how many matched their recorded hash.
    pub intact: usize,
    /// Every blob that did not.
    pub findings: Vec<ScrubFinding>,
    /// Files in the store that no expectation referenced. Reported as a count,
    /// never repaired here: they are GC candidates, and deciding that is not
    /// the scrub's job.
    pub unreferenced: usize,
}

/// @id: 53e12cb2-312c-4322-905d-6ae98891c192
impl ScrubReport {
    /// @id: 9451c2a1-7045-4ef1-a56d-c60e7f67d9f8
    /// True when nothing failed verification.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// @id: 2c16c3a8-bc70-4653-ab9f-d02e0f67a71e
    /// One-line summary for logs.
    pub fn summary(&self) -> String {
        format!(
            "scrub: {} checked, {} intact, {} corrupt, {} unreferenced files",
            self.checked,
            self.intact,
            self.findings.len(),
            self.unreferenced
        )
    }
}

/// @id: 3c93c30c-ca41-41e5-8c53-1858f5521844
/// How much of the store one pass covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubScope {
    /// Every expectation supplied.
    Full,
    /// At most this many, taken from the front of the list. Sampling belongs
    /// to the caller's query (`ORDER BY random() LIMIT n`) so this module needs
    /// no randomness of its own and one pass stays reproducible.
    Sample(usize),
}

/// @id: c5d1da39-fa8e-45eb-bfbd-9176f00377a8
/// Pacing, so a scrub never competes with live I/O.
///
/// Decision point 7 wants this running at low load; a pause between blobs is
/// the cheapest way to guarantee it cannot saturate the disk on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubPacing {
    /// Sleep after each verified blob.
    pub pause_between: Duration,
}

impl Default for ScrubPacing {
    fn default() -> Self {
        Self {
            pause_between: Duration::from_millis(5),
        }
    }
}

/// @id: 7e6a42fb-c936-4a62-9a96-e28246e77acc
/// Verifies stored blobs against their recorded digests.
///
/// `expectations` maps blob id to the `content_hash` the database holds — the
/// SHA-256 of the plaintext, before compression (Root Invariant #1). `decode`
/// turns stored bytes back into that digest; it is injected to avoid a
/// dependency cycle, see the module docs.
///
/// A mismatch is reported, never repaired: the correct bytes are not knowable
/// from here, and silently deleting a corrupt blob would turn detectable rot
/// into a missing file.
pub async fn scrub_once<F>(
    store_root: impl AsRef<Path>,
    expectations: &[(Uuid, String)],
    scope: ScrubScope,
    pacing: ScrubPacing,
    decode: F,
) -> Result<ScrubReport>
where
    F: Fn(&[u8]) -> std::result::Result<String, String>,
{
    let store = LocalDiskStore::new(store_root.as_ref());
    let take = match scope {
        ScrubScope::Full => expectations.len(),
        ScrubScope::Sample(n) => n.min(expectations.len()),
    };

    let mut report = ScrubReport::default();
    let mut expected_ids: HashMap<Uuid, ()> = HashMap::with_capacity(expectations.len());
    for (blob_id, _) in expectations {
        expected_ids.insert(*blob_id, ());
    }

    for (blob_id, expected) in expectations.iter().take(take) {
        let path = store.blob_path(*blob_id);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                report.findings.push(ScrubFinding {
                    blob_id: *blob_id,
                    expected: expected.clone(),
                    actual: ScrubActual::Missing,
                    path,
                });
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        report.checked += 1;
        match decode(&bytes) {
            Ok(actual) if &actual == expected => report.intact += 1,
            Ok(actual) => report.findings.push(ScrubFinding {
                blob_id: *blob_id,
                expected: expected.clone(),
                actual: ScrubActual::Digest(actual),
                path,
            }),
            Err(e) => report.findings.push(ScrubFinding {
                blob_id: *blob_id,
                expected: expected.clone(),
                actual: ScrubActual::Undecodable(e),
                path,
            }),
        }

        if !pacing.pause_between.is_zero() {
            tokio::time::sleep(pacing.pause_between).await;
        }
    }

    report.unreferenced = count_unreferenced(store_root.as_ref(), &expected_ids).await?;
    Ok(report)
}

/// @id: 6b2f43e7-9514-4c15-93da-1dff5ce0d343
/// Counts blob files no expectation mentions.
///
/// Only the store root itself, never `pending/` — those are queue markers, not
/// blobs, and counting them as orphans would report every in-flight write as a
/// leak.
async fn count_unreferenced(root: &Path, expected: &HashMap<Uuid, ()>) -> Result<usize> {
    let mut dir = match fs::read_dir(root).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };

    let mut count = 0usize;
    while let Some(entry) = dir.next_entry().await? {
        if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue; // pending/, and anything else structural
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        match Uuid::parse_str(&name) {
            Ok(id) if !expected.contains_key(&id) => count += 1,
            // Not a blob name at all (a stray file, a .tmp) — not a blob leak.
            Ok(_) | Err(_) => {}
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::BlobStore;
    use tempfile::tempdir;

    /// Stand-in codec: stored bytes are the plaintext reversed, and the
    /// "digest" is the plaintext itself. Keeps the tests about the scrub rather
    /// than about zstd.
    fn decode(bytes: &[u8]) -> std::result::Result<String, String> {
        let text = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
        if text.starts_with("BROKEN") {
            return Err("not decodable".to_string());
        }
        Ok(text.chars().rev().collect())
    }

    fn stored(plain: &str) -> Vec<u8> {
        plain.chars().rev().collect::<String>().into_bytes()
    }

    const NO_PAUSE: ScrubPacing = ScrubPacing {
        pause_between: Duration::ZERO,
    };

    #[tokio::test]
    async fn intact_blobs_verify_clean() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();
        store.put(id, &stored("hello")).await.unwrap();

        let report = scrub_once(
            dir.path(),
            &[(id, "hello".to_string())],
            ScrubScope::Full,
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert!(report.is_clean());
        assert_eq!((report.checked, report.intact), (1, 1));
        assert_eq!(report.unreferenced, 0);
    }

    #[tokio::test]
    async fn rotted_bytes_are_reported_with_both_digests() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();
        // The file says "goodbye" but the database expects "hello".
        store.put(id, &stored("goodbye")).await.unwrap();

        let report = scrub_once(
            dir.path(),
            &[(id, "hello".to_string())],
            ScrubScope::Full,
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.expected, "hello");
        assert_eq!(f.actual, ScrubActual::Digest("goodbye".to_string()));
        assert_eq!(report.intact, 0);
    }

    #[tokio::test]
    async fn a_corrupt_blob_is_reported_not_deleted() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let id = Uuid::new_v4();
        store.put(id, b"BROKEN garbage").await.unwrap();

        let report = scrub_once(
            dir.path(),
            &[(id, "hello".to_string())],
            ScrubScope::Full,
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert!(matches!(
            report.findings[0].actual,
            ScrubActual::Undecodable(_)
        ));
        assert!(
            store.exists(id).await.unwrap(),
            "a corrupt blob must survive the scrub; deleting it would turn \
             detectable rot into a missing file"
        );
    }

    #[tokio::test]
    async fn a_referenced_but_absent_blob_is_reported_missing() {
        let dir = tempdir().unwrap();
        let id = Uuid::new_v4();

        let report = scrub_once(
            dir.path(),
            &[(id, "hello".to_string())],
            ScrubScope::Full,
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert_eq!(report.findings[0].actual, ScrubActual::Missing);
        assert_eq!(report.checked, 0, "a file that is not there was not checked");
    }

    #[tokio::test]
    async fn sampling_limits_the_pass() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let mut expectations = Vec::new();
        for i in 0..5 {
            let id = Uuid::new_v4();
            let plain = format!("body {i}");
            store.put(id, &stored(&plain)).await.unwrap();
            expectations.push((id, plain));
        }

        let report = scrub_once(
            dir.path(),
            &expectations,
            ScrubScope::Sample(2),
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert_eq!(report.checked, 2);
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn pending_markers_are_not_counted_as_orphan_blobs() {
        let dir = tempdir().unwrap();
        let store = LocalDiskStore::new(dir.path());
        let known = Uuid::new_v4();
        store.put(known, &stored("kept")).await.unwrap();

        // An in-flight write, plus a genuinely unreferenced blob.
        let queue = crate::pending::PendingQueue::new(dir.path());
        queue.ensure_dirs().await.unwrap();
        std::fs::write(queue.queue_dir().join("marker"), b"x").unwrap();
        let orphan = Uuid::new_v4();
        store.put(orphan, &stored("orphan")).await.unwrap();

        let report = scrub_once(
            dir.path(),
            &[(known, "kept".to_string())],
            ScrubScope::Full,
            NO_PAUSE,
            decode,
        )
        .await
        .unwrap();

        assert_eq!(
            report.unreferenced, 1,
            "only the orphan blob counts; pending/ holds queue markers, and \
             counting those would report every in-flight write as a leak"
        );
    }
}
