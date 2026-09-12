use serde::{Deserialize, Serialize};
pub use uuid::Uuid;

/// @id: 4afea11b-137e-4b83-90be-1e9a95a18295
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

/// @id: c0245132-2678-4ba3-9cd8-119c37340d31
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

/// @id: 2337ca78-2cf5-44b4-adfa-3ba20a2214db
/// POSIX file mode newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileMode(pub u32);

/// @id: 3947c915-c755-4d6b-9718-9a2947f87916
impl From<u32> for FileMode {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

/// @id: 39401276-d8c2-4b6b-80ad-332c5e50d8a3
impl From<FileMode> for u32 {
    fn from(m: FileMode) -> Self {
        m.0
    }
}

/// @id: 181447ec-e31f-46eb-b9bc-4d98f16a35f2
/// POSIX user ID newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Uid(pub u32);

/// @id: 8702d3a4-0358-406c-b0e3-9f5f7e87fb69
impl From<u32> for Uid {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

/// @id: f1b82bcd-b6b8-4ee0-84ec-c4b6dfcc15d2
impl From<Uid> for u32 {
    fn from(u: Uid) -> Self {
        u.0
    }
}

/// @id: 33afc30f-503f-4355-a60f-ac5a516bcd07
/// POSIX group ID newtype wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Gid(pub u32);

/// @id: da196001-0592-4fac-9ca5-c17066262c7a
impl From<u32> for Gid {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

/// @id: 0e0d778b-d7a5-493d-997d-40e9f13c3149
impl From<Gid> for u32 {
    fn from(g: Gid) -> Self {
        g.0
    }
}

/// @id: 605205f8-394b-4172-b015-d3936f2a4cb8
/// Blob identifier in storage backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobId(pub Uuid);

/// @id: 2106f57a-f5f3-48fb-9c61-c8eaf3d15e84
impl From<Uuid> for BlobId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// @id: a06497e9-19d1-4091-adbf-d4eb05311b06
impl From<BlobId> for Uuid {
    fn from(b: BlobId) -> Self {
        b.0
    }
}

/// @id: 82006ab1-2054-4eaa-a1f5-14e168d9881a
impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// @id: 6ed57013-b303-442a-8b04-ab2f8a1caab3
/// Content hash (hex-encoded SHA-256 computed before compression, per Invariant #1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub String);

/// @id: 8cb188d2-d61b-41c3-8d65-6710b793d29e
impl From<String> for ContentHash {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// @id: aa217a1e-013f-4f81-bbaf-ab9d03fadbcb
impl From<&str> for ContentHash {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// @id: 67600af7-beda-4acb-b6d1-c01b25e36d7a
impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// @id: 6fcf5695-8e4c-4f9a-a4ea-ad4e0ebe6294
/// Monotonically increasing file version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VersionNumber(pub i32);

/// @id: e8ea2304-b67e-4615-8d32-e2fda6907a29
impl From<i32> for VersionNumber {
    fn from(v: i32) -> Self {
        Self(v)
    }
}

/// @id: 9cbb6512-aa67-4e96-97fd-7a86cd9f701f
impl From<VersionNumber> for i32 {
    fn from(v: VersionNumber) -> Self {
        v.0
    }
}

/// @id: f6652844-c4e3-4b79-93af-518de00dee58
impl std::fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
