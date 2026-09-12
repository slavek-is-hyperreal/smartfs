//! Command-line argument specifications for `smartfs-cli`.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use uuid::Uuid;

/// @id: e6a1b2c3-1001-4000-8000-000000000001
/// Main CLI structure for SmartFS CLI.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "smartfs-cli",
    about = "Command-line interface for SmartFS",
    version
)]
pub struct Cli {
    /// PostgreSQL database connection URL (falls back to DATABASE_URL env var or default).
    #[arg(long, global = true)]
    pub database_url: Option<String>,

    /// Storage directory path for local disk store blobs (falls back to SMARTFS_STORE_PATH env var or default).
    #[arg(long, global = true)]
    pub store_path: Option<PathBuf>,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// @id: a362ce60-77c7-4f18-9cd3-c55ba2c2de6c
impl Cli {
    /// @id: e6a1b2c3-1001-4000-8000-000000000002
    /// Resolves the database connection URL from argument, environment variable, or default.
    pub fn database_url(&self) -> String {
        self.database_url
            .clone()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .unwrap_or_else(|| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
    }

    /// @id: e6a1b2c3-1001-4000-8000-000000000003
    /// Resolves the storage path from argument, environment variable, or default.
    pub fn store_path(&self) -> PathBuf {
        self.store_path
            .clone()
            .or_else(|| std::env::var("SMARTFS_STORE_PATH").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/var/lib/smartfs/blobs"))
    }
}

/// @id: e6a1b2c3-1002-4000-8000-000000000002
/// Available subcommands for `smartfs-cli`.
#[derive(Subcommand, Debug, Clone, PartialEq)]
pub enum Commands {
    /// Write file content to SmartFS.
    Write(WriteArgs),

    /// Print file content from SmartFS.
    Cat(CatArgs),

    /// Display version history for a file.
    History(HistoryArgs),

    /// Search fulltext BM25 in file versions and AST nodes.
    Search(SearchArgs),

    /// Show AST node differences between two file versions.
    Diff(DiffArgs),

    /// Register or import an existing directory into SmartFS.
    Import(ImportArgs),

    /// List active concept centroids and semantic clusters.
    Concepts(ConceptsArgs),

    /// Calibrate join_threshold for semantic consolidation.
    Calibrate(CalibrateArgs),

    /// Display system status, pending backlog, and unconsolidated counts.
    Status(StatusArgs),
}

/// @id: e6a1b2c3-1003-4000-8000-000000000003
/// Arguments for `write` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct WriteArgs {
    /// Path inside SmartFS (e.g. /src/main.rs).
    pub path: String,

    /// Optional local file to read content from; if omitted, reads from standard input.
    pub file_to_read: Option<PathBuf>,

    /// Override syntax validation checks and mark file version as 'syntax_error'.
    #[arg(long, short = 'f')]
    pub force: bool,
}

/// @id: e6a1b2c3-1004-4000-8000-000000000004
/// Arguments for `cat` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct CatArgs {
    /// Path of the file inside SmartFS.
    pub path: String,

    /// Specific version number to retrieve (defaults to latest version).
    #[arg(long, short = 'v')]
    pub version: Option<i32>,
}

/// @id: e6a1b2c3-1005-4000-8000-000000000005
/// Arguments for `history` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct HistoryArgs {
    /// Path of the file inside SmartFS.
    pub path: String,
}

/// @id: e6a1b2c3-1006-4000-8000-000000000006
/// Arguments for `search` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct SearchArgs {
    /// Search query string.
    pub query: String,

    /// Optional plugin type filter (e.g. rust, python, generic).
    #[arg(long)]
    pub plugin_type: Option<String>,

    /// Maximum number of search results to return.
    #[arg(long, default_value = "20")]
    pub limit: i64,
}

/// @id: e6a1b2c3-1007-4000-8000-000000000007
/// Arguments for `diff` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct DiffArgs {
    /// Path of the file inside SmartFS.
    pub path: String,

    /// First version number.
    pub v1: i32,

    /// Second version number.
    pub v2: i32,
}

/// @id: e6a1b2c3-1008-4000-8000-000000000008
/// Import mode for registering existing directories.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// In-place index registration with external_path (does not copy blob into store).
    Index,
    /// Copy-on-Write ingestion (compresses and writes blob into store).
    Cow,
}

/// @id: e6a1b2c3-1009-4000-8000-000000000009
/// Arguments for `import` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct ImportArgs {
    /// Directory to import.
    pub dir: PathBuf,

    /// Import mode: 'index' (in-place external_path) or 'cow' (full ingestion to store).
    #[arg(long, value_enum, default_value = "cow")]
    pub mode: ImportMode,
}

/// @id: e6a1b2c3-1010-4000-8000-000000000010
/// Arguments for `concepts` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct ConceptsArgs {
    /// Filter by plugin type (e.g. rust, python, generic).
    #[arg(long)]
    pub plugin_type: Option<String>,

    /// Maximum number of concepts to return.
    #[arg(long)]
    pub limit: Option<usize>,
}

/// @id: e6a1b2c3-1011-4000-8000-000000000011
/// Arguments for `calibrate` command.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct CalibrateArgs {
    /// Plugin type to calibrate (e.g. rust, python, generic).
    #[arg(long)]
    pub plugin_type: String,

    /// Model UUID to calibrate.
    #[arg(long)]
    pub model_id: Uuid,

    /// Target percentile or empirical join threshold to calibrate (e.g. 0.15).
    #[arg(long)]
    pub target_percentile: Option<f64>,
}

/// @id: e6a1b2c3-1012-4000-8000-000000000012
/// Arguments for `status` command.
#[derive(Args, Debug, Clone, PartialEq, Default)]
pub struct StatusArgs {}
