//! Clap command-line parsing unit tests for `smartfs-cli`.

use clap::Parser;
use smartfs_cli::args::{
    CalibrateArgs, CatArgs, Cli, Commands, ConceptsArgs, DiffArgs, HistoryArgs, ImportArgs,
    ImportMode, SearchArgs, StatusArgs, WriteArgs,
};
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn test_parse_write_basic() {
    let cli = Cli::try_parse_from(["smartfs-cli", "write", "/docs/readme.md"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::Write(WriteArgs {
            path: "/docs/readme.md".to_string(),
            file_to_read: None,
            force: false,
        })
    );
}

#[test]
fn test_parse_write_with_file_and_force() {
    let cli = Cli::try_parse_from([
        "smartfs-cli",
        "write",
        "--force",
        "/src/main.rs",
        "local_main.rs",
    ])
    .unwrap();
    assert_eq!(
        cli.command,
        Commands::Write(WriteArgs {
            path: "/src/main.rs".to_string(),
            file_to_read: Some(PathBuf::from("local_main.rs")),
            force: true,
        })
    );
}

#[test]
fn test_parse_cat() {
    let cli = Cli::try_parse_from(["smartfs-cli", "cat", "/src/lib.rs"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::Cat(CatArgs {
            path: "/src/lib.rs".to_string(),
            version: None,
        })
    );

    let cli_ver = Cli::try_parse_from(["smartfs-cli", "cat", "/src/lib.rs", "--version", "3"]).unwrap();
    assert_eq!(
        cli_ver.command,
        Commands::Cat(CatArgs {
            path: "/src/lib.rs".to_string(),
            version: Some(3),
        })
    );
}

#[test]
fn test_parse_history() {
    let cli = Cli::try_parse_from(["smartfs-cli", "history", "/src/lib.rs"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::History(HistoryArgs {
            path: "/src/lib.rs".to_string(),
        })
    );
}

#[test]
fn test_parse_search() {
    let cli = Cli::try_parse_from(["smartfs-cli", "search", "BlobStore"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::Search(SearchArgs {
            query: "BlobStore".to_string(),
            plugin_type: None,
            limit: 20,
        })
    );

    let cli_filtered = Cli::try_parse_from([
        "smartfs-cli",
        "search",
        "fn main",
        "--plugin-type",
        "rust",
        "--limit",
        "5",
    ])
    .unwrap();
    assert_eq!(
        cli_filtered.command,
        Commands::Search(SearchArgs {
            query: "fn main".to_string(),
            plugin_type: Some("rust".to_string()),
            limit: 5,
        })
    );
}

#[test]
fn test_parse_diff() {
    let cli = Cli::try_parse_from(["smartfs-cli", "diff", "/src/main.rs", "1", "2"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::Diff(DiffArgs {
            path: "/src/main.rs".to_string(),
            v1: 1,
            v2: 2,
        })
    );
}

#[test]
fn test_parse_import() {
    let cli_default = Cli::try_parse_from(["smartfs-cli", "import", "/tmp/docs"]).unwrap();
    assert_eq!(
        cli_default.command,
        Commands::Import(ImportArgs {
            dir: PathBuf::from("/tmp/docs"),
            mode: ImportMode::Cow,
        })
    );

    let cli_index = Cli::try_parse_from([
        "smartfs-cli",
        "import",
        "--mode",
        "index",
        "/tmp/docs",
    ])
    .unwrap();
    assert_eq!(
        cli_index.command,
        Commands::Import(ImportArgs {
            dir: PathBuf::from("/tmp/docs"),
            mode: ImportMode::Index,
        })
    );
}

#[test]
fn test_parse_concepts() {
    let cli = Cli::try_parse_from(["smartfs-cli", "concepts"]).unwrap();
    assert_eq!(
        cli.command,
        Commands::Concepts(ConceptsArgs {
            plugin_type: None,
            limit: None,
        })
    );

    let cli_filtered = Cli::try_parse_from([
        "smartfs-cli",
        "concepts",
        "--plugin-type",
        "rust",
        "--limit",
        "10",
    ])
    .unwrap();
    assert_eq!(
        cli_filtered.command,
        Commands::Concepts(ConceptsArgs {
            plugin_type: Some("rust".to_string()),
            limit: Some(10),
        })
    );
}

#[test]
fn test_parse_calibrate() {
    let model_id = Uuid::new_v4();
    let model_str = model_id.to_string();

    let cli = Cli::try_parse_from([
        "smartfs-cli",
        "calibrate",
        "--plugin-type",
        "rust",
        "--model-id",
        &model_str,
    ])
    .unwrap();

    assert_eq!(
        cli.command,
        Commands::Calibrate(CalibrateArgs {
            plugin_type: "rust".to_string(),
            model_id,
            target_percentile: None,
        })
    );

    let cli_pct = Cli::try_parse_from([
        "smartfs-cli",
        "calibrate",
        "--plugin-type",
        "python",
        "--model-id",
        &model_str,
        "--target-percentile",
        "0.20",
    ])
    .unwrap();

    assert_eq!(
        cli_pct.command,
        Commands::Calibrate(CalibrateArgs {
            plugin_type: "python".to_string(),
            model_id,
            target_percentile: Some(0.20),
        })
    );
}

#[test]
fn test_parse_status() {
    let cli = Cli::try_parse_from(["smartfs-cli", "status"]).unwrap();
    assert_eq!(cli.command, Commands::Status(StatusArgs {}));
}

#[test]
fn test_parse_global_options() {
    let cli = Cli::try_parse_from([
        "smartfs-cli",
        "--database-url",
        "postgres://user:pass@localhost:5432/mydb",
        "--store-path",
        "/custom/store",
        "status",
    ])
    .unwrap();

    assert_eq!(
        cli.database_url(),
        "postgres://user:pass@localhost:5432/mydb"
    );
    assert_eq!(cli.store_path(), PathBuf::from("/custom/store"));
    assert_eq!(cli.command, Commands::Status(StatusArgs {}));
}

#[test]
fn test_parse_missing_subcommand_fails() {
    let res = Cli::try_parse_from(["smartfs-cli"]);
    assert!(res.is_err());
}
