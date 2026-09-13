//! Error mapping utilities between SmartFsError and libc errno codes.

use smartfs_schema::error::SmartFsError;

/// @id: a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d
/// Converts a `SmartFsError` into a POSIX libc error number (`c_int`).
pub fn error_to_errno(err: &SmartFsError) -> libc::c_int {
    match err {
        SmartFsError::NotFound(_) => libc::ENOENT,
        SmartFsError::Conflict(_) => libc::EEXIST,
        SmartFsError::SyntaxError(_) => libc::EACCES,
        SmartFsError::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
        // ADR-58 point 6: back-pressure, not data loss. EAGAIN tells the caller
        // the write was refused and may be retried — it never means a write was
        // accepted and then dropped.
        SmartFsError::PendingQueueFull => libc::EAGAIN,
        _ => libc::EIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_mapping() {
        assert_eq!(
            error_to_errno(&SmartFsError::NotFound("missing".into())),
            libc::ENOENT
        );
        assert_eq!(
            error_to_errno(&SmartFsError::Conflict("exists".into())),
            libc::EEXIST
        );
        assert_eq!(
            error_to_errno(&SmartFsError::SyntaxError("parse error".into())),
            libc::EACCES
        );
        assert_eq!(
            error_to_errno(&SmartFsError::Db("query failed".into())),
            libc::EIO
        );
        assert_eq!(
            error_to_errno(&SmartFsError::PendingQueueFull),
            libc::EAGAIN,
            "a full queue must be retryable back-pressure, never a generic I/O error"
        );
    }
}
