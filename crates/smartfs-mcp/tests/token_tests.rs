use std::time::Duration;
use smartfs_mcp::tokens::{DestructiveAction, TokenManager};

#[test]
fn test_token_creation_and_consumption() {
    let mut manager = TokenManager::new(Duration::from_secs(60));

    let action = DestructiveAction::DeleteFile {
        path: "/mnt/smartfs/docs/test.txt".to_string(),
    };

    let token = manager.create_token(action.clone());
    assert!(!token.is_empty());

    // Valid consumption
    let consumed = manager.consume_token(&token, "delete_file", "/mnt/smartfs/docs/test.txt");
    assert_eq!(consumed, Some(action));

    // Consumed token cannot be reused
    let second_try = manager.consume_token(&token, "delete_file", "/mnt/smartfs/docs/test.txt");
    assert_eq!(second_try, None);
}

#[test]
fn test_token_consumption_mismatched_action_fails() {
    let mut manager = TokenManager::new(Duration::from_secs(60));

    let action = DestructiveAction::DeleteFile {
        path: "/mnt/smartfs/docs/test.txt".to_string(),
    };

    let token = manager.create_token(action);

    // Mismatched action name
    let failed = manager.consume_token(&token, "overwrite_file", "/mnt/smartfs/docs/test.txt");
    assert_eq!(failed, None);

    // Mismatched path
    let failed_path = manager.consume_token(&token, "delete_file", "/mnt/smartfs/other.txt");
    assert_eq!(failed_path, None);
}

#[test]
fn test_token_expiration() {
    let mut manager = TokenManager::new(Duration::from_millis(50));

    let action = DestructiveAction::OverwriteFile {
        path: "/mnt/smartfs/src/main.rs".to_string(),
        content: "fn main() {}".to_string(),
    };

    let token = manager.create_token(action);
    std::thread::sleep(Duration::from_millis(100));

    // Token has expired
    let expired = manager.consume_token(&token, "overwrite_file", "/mnt/smartfs/src/main.rs");
    assert_eq!(expired, None);
}
