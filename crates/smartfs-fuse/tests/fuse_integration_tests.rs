//! Integration tests for smartfs-fuse state management, buffer lifecycle,
//! delayed unlinking, AST syntax validation, and POSIX attribute conversions.

use std::sync::Arc;
use std::time::SystemTime;

use chrono::Utc;
use fuser::FileType;
use smartfs_db::InodeRecord;
use smartfs_fuse::{
    default_mount_options, detect_syntax_language, error_to_errno, extract_ast_nodes,
    inode_to_file_attr, system_time_from_datetime, validate_and_extract_ast_blocking,
    validate_syntax, FuseStateManager,
};
use smartfs_schema::error::SmartFsError;
use uuid::Uuid;

#[test]
fn test_fuse_mount_options() {
    let opts = default_mount_options();
    assert!(opts.contains(&fuser::MountOption::DefaultPermissions));
    assert!(opts.contains(&fuser::MountOption::AllowOther));
    assert!(opts.contains(&fuser::MountOption::AutoUnmount));
}

#[test]
fn test_inode_attribute_conversion_invariants() {
    let now = Utc::now();
    let file_id = Uuid::new_v4();

    // 1. Regular file record
    let file_record = InodeRecord {
        id: file_id,
        ino: 42,
        parent_id: Some(Uuid::new_v4()),
        name: "test.rs".to_string(),
        is_dir: false,
        uid: 1001,
        gid: 1001,
        mode: 0o100644,
        size: 5000,
        nlink: 1,
        created_at: now,
        updated_at: now,
        current_blob_id: Some(Uuid::new_v4()),
        backend_id: None,
        on_prem: true,
        compression_level: 3,
        versioning_enabled: true,
    };

    let file_attr = inode_to_file_attr(&file_record);
    assert_eq!(file_attr.ino, 42);
    assert_eq!(file_attr.size, 5000);
    assert_eq!(file_attr.blocks, 5000u64.div_ceil(512));
    assert_eq!(file_attr.kind, FileType::RegularFile);
    assert_eq!(file_attr.perm, 0o644);
    assert_eq!(file_attr.nlink, 1);
    assert_eq!(file_attr.uid, 1001);
    assert_eq!(file_attr.gid, 1001);
    assert_eq!(file_attr.blksize, 4096);

    // 2. Directory record
    let dir_record = InodeRecord {
        id: Uuid::new_v4(),
        ino: 1,
        parent_id: None,
        name: "".to_string(),
        is_dir: true,
        uid: 0,
        gid: 0,
        mode: 0o40755,
        size: 4096,
        nlink: 1,
        created_at: now,
        updated_at: now,
        current_blob_id: None,
        backend_id: None,
        on_prem: true,
        compression_level: 3,
        versioning_enabled: true,
    };

    let dir_attr = inode_to_file_attr(&dir_record);
    assert_eq!(dir_attr.ino, 1);
    assert_eq!(dir_attr.kind, FileType::Directory);
    assert_eq!(dir_attr.perm, 0o755);
    assert_eq!(dir_attr.nlink, 2);

    // 3. Negative size invariant (never set -1 as size, §3.6)
    let bad_size_record = InodeRecord {
        size: -1,
        ..file_record
    };
    let bad_attr = inode_to_file_attr(&bad_size_record);
    assert_eq!(bad_attr.size, 0);
}

#[test]
fn test_system_time_conversion() {
    let now = Utc::now();
    let sys_time = system_time_from_datetime(&now);
    assert!(sys_time <= SystemTime::now());
}

#[test]
fn test_error_to_errno_mapping() {
    assert_eq!(
        error_to_errno(&SmartFsError::NotFound("lost".into())),
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
        error_to_errno(&SmartFsError::Io(std::io::Error::from_raw_os_error(
            libc::EBUSY
        ))),
        libc::EBUSY
    );
    assert_eq!(
        error_to_errno(&SmartFsError::Other("unknown".into())),
        libc::EIO
    );
}

#[test]
fn test_state_manager_buffer_lifecycle() {
    let state = Arc::new(FuseStateManager::new());
    let inode_id = Uuid::new_v4();
    let ino = 100u64;

    // Allocate handle with initial buffer
    let fh = state.allocate_fh(ino, inode_id, 0, Some(b"Hello FUSE".to_vec()));
    assert_eq!(fh, 1);
    assert_eq!(state.open_fd_count(inode_id), 1);

    let handle = state.get_handle(fh).expect("handle should exist");
    assert_eq!(handle.buffer.unwrap(), b"Hello FUSE");
    assert!(handle.modified);

    // Random access write
    let n = state
        .write_to_handle(fh, 6, b"SmartFS World!")
        .expect("write to handle");
    assert_eq!(n, 14);

    let handle_after_write = state.get_handle(fh).unwrap();
    assert_eq!(
        handle_after_write.buffer.unwrap(),
        b"Hello SmartFS World!"
    );

    // Truncate down
    state.truncate_handle(fh, 5).expect("truncate down");
    let handle_truncated = state.get_handle(fh).unwrap();
    assert_eq!(handle_truncated.buffer.unwrap(), b"Hello");

    // Truncate up (zero-fills)
    state.truncate_handle(fh, 8).expect("truncate up");
    let handle_expanded = state.get_handle(fh).unwrap();
    assert_eq!(handle_expanded.buffer.unwrap(), b"Hello\0\0\0");

    // Close handle
    let (closed, should_delete) = state.close_handle(fh).expect("close handle");
    assert_eq!(closed.fh, fh);
    assert!(!should_delete);
    assert_eq!(state.open_fd_count(inode_id), 0);
}

#[test]
fn test_delayed_unlink_concurrent_handles() {
    let state = Arc::new(FuseStateManager::new());
    let inode_id = Uuid::new_v4();
    let ino = 200u64;

    // Simulate two concurrent opens on the same file
    let fh1 = state.allocate_fh(ino, inode_id, 0, None);
    let fh2 = state.allocate_fh(ino, inode_id, 0, None);
    assert_eq!(state.open_fd_count(inode_id), 2);

    // Unlink file while open
    let immediate = state.mark_for_deletion(inode_id);
    assert!(
        !immediate,
        "Unlink must be delayed when file handles are still open"
    );
    assert!(state.is_marked_for_deletion(inode_id));

    // Close first handle
    let (_, should_delete1) = state.close_handle(fh1).unwrap();
    assert!(
        !should_delete1,
        "Must NOT delete while other handle is still open"
    );
    assert_eq!(state.open_fd_count(inode_id), 1);

    // Close second handle (last open fd)
    let (_, should_delete2) = state.close_handle(fh2).unwrap();
    assert!(
        should_delete2,
        "Delayed deletion MUST trigger when open_fd_count reaches 0"
    );
    assert_eq!(state.open_fd_count(inode_id), 0);
}

#[test]
fn test_immediate_unlink_when_no_handles_open() {
    let state = Arc::new(FuseStateManager::new());
    let inode_id = Uuid::new_v4();

    // Inode has no open handles
    let immediate = state.mark_for_deletion(inode_id);
    assert!(
        immediate,
        "Must return true for immediate deletion when open_fd_count == 0"
    );
}

#[test]
fn test_syntax_validation_and_ast_parsing() {
    assert_eq!(detect_syntax_language("main.rs"), Some("rust"));
    assert_eq!(detect_syntax_language("worker.py"), Some("python"));
    assert_eq!(detect_syntax_language("config.json"), Some("json"));
    assert_eq!(detect_syntax_language("README.txt"), None);

    // Valid Rust code
    let valid_rust = r#"
        fn compute(a: i32, b: i32) -> i32 {
            a * b
        }

        struct Point {
            x: f64,
            y: f64,
        }
    "#;
    assert!(validate_syntax(valid_rust, "rust").is_ok());

    let nodes = extract_ast_nodes(valid_rust, "rust");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].kind, "function");
    assert_eq!(nodes[0].name, "compute");
    assert_eq!(nodes[1].kind, "struct");
    assert_eq!(nodes[1].name, "Point");

    // Syntax error: unmatched parenthesis and open brace (§11.2)
    let invalid_rust = "fn broken( {";
    let res = validate_syntax(invalid_rust, "rust");
    assert!(res.is_err());
    match res.unwrap_err() {
        SmartFsError::SyntaxError(msg) => {
            assert!(msg.contains("Unclosed delimiter") || msg.contains("Mismatched"));
        }
        other => panic!("Expected SyntaxError, got {:?}", other),
    }

    // Invalid JSON
    let invalid_json = "{ name: without_quotes }";
    assert!(validate_syntax(invalid_json, "json").is_err());
}

#[tokio::test]
async fn test_spawn_blocking_ast_validation() {
    let code = r#"
        pub fn run_task() -> bool {
            true
        }
    "#
    .to_string();

    let nodes = validate_and_extract_ast_blocking(code, "test_file.rs".to_string())
        .await
        .expect("validation should succeed");

    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, "function");
    assert_eq!(nodes[0].name, "run_task");
    assert!(!nodes[0].content_hash.is_empty());

    // Syntax error offloaded to spawn_blocking must return SyntaxError
    let broken_code = "fn broken( {".to_string();
    let broken_res = validate_and_extract_ast_blocking(broken_code, "test_file.rs".to_string()).await;
    assert!(broken_res.is_err());
    assert!(matches!(broken_res.unwrap_err(), SmartFsError::SyntaxError(_)));
}
