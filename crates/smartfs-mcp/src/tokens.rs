//! Confirmation token management for destructive operations (ADR-48).

use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// @id: a77829bb-f8aa-48a9-8541-3d4517a2e974
/// A destructive operation that requires confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestructiveAction {
    DeleteFile { path: String },
    OverwriteFile { path: String, content: String },
}

/// @id: 5ea60772-bd87-4c28-9314-5abe4f4fb46f
impl DestructiveAction {
    /// @id: f687a482-ee20-48d9-9cdb-3b93ce4f0c54
    /// Action name identifier.
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::DeleteFile { .. } => "delete_file",
            Self::OverwriteFile { .. } => "overwrite_file",
        }
    }

    /// @id: 7589c6d3-ce29-4151-9756-336103a9f048
    /// Target path of the destructive action.
    pub fn path(&self) -> &str {
        match self {
            Self::DeleteFile { path } => path,
            Self::OverwriteFile { path, .. } => path,
        }
    }
}

/// @id: 744d99eb-dc7c-4022-9940-4c9391f1edcf
/// Record of a pending confirmation token.
#[derive(Debug, Clone)]
pub struct PendingConfirmation {
    pub token: String,
    pub action: DestructiveAction,
    pub expires_at: Instant,
}

/// @id: fdb3597d-ce98-49f0-ad73-df177743fb3b
/// In-memory manager for confirmation tokens.
#[derive(Debug)]
pub struct TokenManager {
    tokens: HashMap<String, PendingConfirmation>,
    ttl: Duration,
}

/// @id: ca4f9cf4-6784-43fe-ac9b-a9d9d41b7ad9
impl TokenManager {
    /// @id: e78d7cfd-f109-4c7d-868e-f9669056a93f
    /// Creates a new `TokenManager` with the specified token time-to-live.
    pub fn new(ttl: Duration) -> Self {
        Self {
            tokens: HashMap::new(),
            ttl,
        }
    }

    /// @id: cbff604d-5295-4c39-bcee-211036d7f9c3
    /// Default manager with 5-minute TTL.
    pub fn with_default_ttl() -> Self {
        Self::new(Duration::from_secs(300))
    }

    /// @id: bf6d4d8e-e1c5-4b5e-8dae-9ad179f36697
    /// Generates a new confirmation token for the given destructive action.
    pub fn create_token(&mut self, action: DestructiveAction) -> String {
        self.cleanup_expired();
        let token = Uuid::new_v4().to_string();
        let expires_at = Instant::now() + self.ttl;
        self.tokens.insert(
            token.clone(),
            PendingConfirmation {
                token: token.clone(),
                action,
                expires_at,
            },
        );
        token
    }

    /// @id: f6be5291-8e79-44dd-a988-dbf936f54423
    /// Validates, consumes, and removes the token if it matches the expected action and path.
    pub fn consume_token(
        &mut self,
        token: &str,
        expected_action: &str,
        expected_path: &str,
    ) -> Option<DestructiveAction> {
        self.cleanup_expired();
        let pending = self.tokens.get(token)?;
        if pending.action.action_name() != expected_action || pending.action.path() != expected_path {
            return None;
        }
        self.tokens.remove(token).map(|p| p.action)
    }

    /// Cleans up expired confirmation tokens.
    fn cleanup_expired(&mut self) {
        let now = Instant::now();
        self.tokens.retain(|_, v| v.expires_at > now);
    }
}
