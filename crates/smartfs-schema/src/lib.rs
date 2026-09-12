//! smartfs-schema — Shared types and error definitions for SmartFS.
//!
//! Owns exclusively all types shared between crates.
//! Invariant: Zero business logic, zero I/O, zero SQL.

pub mod error;
pub mod types;

pub use error::{Result, SmartFsError};
pub use types::{
    BlobId, ContentHash, FileMode, Gid, ProcessingStatus, Uid, Uuid, VersionNumber,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_status_display() {
        assert_eq!(ProcessingStatus::Clean.to_string(), "clean");
        assert_eq!(ProcessingStatus::Pending.to_string(), "pending");
        assert_eq!(ProcessingStatus::Processing.to_string(), "processing");
        assert_eq!(ProcessingStatus::Failed.to_string(), "failed");
        assert_eq!(ProcessingStatus::SyntaxError.to_string(), "syntax_error");
    }

    #[test]
    fn test_newtypes_conversions() {
        let mode = FileMode::from(0o100644);
        assert_eq!(u32::from(mode), 0o100644);

        let uid = Uid::from(1000);
        assert_eq!(u32::from(uid), 1000);

        let gid = Gid::from(1000);
        assert_eq!(u32::from(gid), 1000);

        let uuid = Uuid::new_v4();
        let blob_id = BlobId::from(uuid);
        assert_eq!(Uuid::from(blob_id), uuid);
        assert_eq!(blob_id.to_string(), uuid.to_string());

        let hash_str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let content_hash = ContentHash::from(hash_str);
        assert_eq!(content_hash.to_string(), hash_str);

        let version = VersionNumber::from(42);
        assert_eq!(i32::from(version), 42);
        assert_eq!(version.to_string(), "42");
    }

    #[test]
    fn test_error_variants() {
        let err = SmartFsError::MissingCalibration {
            plugin_type: "rust".to_string(),
            model_id: Uuid::nil(),
        };
        assert!(err.to_string().contains("Missing calibration threshold"));

        let err2 = SmartFsError::AdvisoryLockUnavailable;
        assert!(err2.to_string().contains("Advisory lock unavailable"));
    }
}
