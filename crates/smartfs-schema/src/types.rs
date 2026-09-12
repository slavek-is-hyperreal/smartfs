use serde::{Deserialize, Serialize};
pub use uuid::Uuid;

/// Mirror of database `processing_status` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    Clean,
    Pending,
    Processing,
    Failed,
    SyntaxError,
}

impl std::fmt::Display for ProcessingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clean => write!(f, "clean"),
            Self::Pending => write!(f, "pending"),
            Self::Processing => write!(f, "processing"),
            Self::Failed => write!(f, "failed"),
            Self::SyntaxError => write!(f, "syntax_error"),
        }
    }
}

/// POSIX file mode newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileMode(pub u32);

impl From<u32> for FileMode {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<FileMode> for u32 {
    fn from(m: FileMode) -> Self {
        m.0
    }
}

/// POSIX user ID newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Uid(pub u32);

impl From<u32> for Uid {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<Uid> for u32 {
    fn from(u: Uid) -> Self {
        u.0
    }
}

/// POSIX group ID newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Gid(pub u32);

impl From<u32> for Gid {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

impl From<Gid> for u32 {
    fn from(g: Gid) -> Self {
        g.0
    }
}

/// Blob identifier in storage backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub Uuid);

impl From<Uuid> for BlobId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<BlobId> for Uuid {
    fn from(b: BlobId) -> Self {
        b.0
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Content hash (hex-encoded SHA-256 computed before compression, per Invariant #1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl From<String> for ContentHash {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ContentHash {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Monotonically increasing file version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VersionNumber(pub i32);

impl From<i32> for VersionNumber {
    fn from(v: i32) -> Self {
        Self(v)
    }
}

impl From<VersionNumber> for i32 {
    fn from(v: VersionNumber) -> Self {
        v.0
    }
}

impl std::fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
