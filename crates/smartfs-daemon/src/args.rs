//! Command-line argument specification for `smartfsd`.
//!
//! Flags and defaults follow docs/testing/the-great-smartfs-test.md §1.2 exactly.
//! The `--database-url` / `--store-path` defaults are deliberately identical to
//! `smartfs-cli`'s, including the environment-variable fallbacks, so both tools
//! address the same system by default.

use std::path::PathBuf;

use clap::Parser;

/// @id: 71cae0d3-bc0d-4783-ba7a-d8dc8eecf854
/// `smartfsd` command-line arguments (§1.2).
#[derive(Parser, Debug, Clone)]
#[command(
    name = "smartfsd",
    about = "SmartFS FUSE daemon — mounts SmartFS at a real path",
    version
)]
pub struct DaemonArgs {
    /// Directory to mount SmartFS on. Must exist, be a directory, be empty and
    /// not already be a mountpoint.
    #[arg(long, required_unless_present = "crash_points")]
    pub mountpoint: Option<PathBuf>,

    /// PostgreSQL connection URL (falls back to DATABASE_URL, then the default).
    #[arg(long)]
    pub database_url: Option<String>,

    /// Blob store root directory (falls back to SMARTFS_STORE_PATH, then the default).
    #[arg(long)]
    pub store_path: Option<PathBuf>,

    /// Add `MountOption::AllowOther`; xfstests needs it when running as a different uid.
    #[arg(long, default_value_t = false)]
    pub allow_other: bool,

    /// Skip the `smartfs-semantic` consolidation supervisors entirely.
    #[arg(long, default_value_t = false)]
    pub no_semantic: bool,

    /// File to write the daemon pid into, after the mount is proven to serve.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,

    /// Readiness file, written only after step 10 of the startup sequence succeeds.
    #[arg(long)]
    pub ready_file: Option<PathBuf>,

    /// `tracing_subscriber::EnvFilter` directive. `RUST_LOG`, when set, wins.
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Never daemonize. Stage 4 kills the process directly, so this is the default.
    #[arg(long, default_value_t = true, action = clap::ArgAction::SetTrue)]
    pub foreground: bool,

    /// Print the crash-point labels compiled into this binary and exit (§1.5).
    #[arg(long, default_value_t = false)]
    pub crash_points: bool,
}

/// @id: 6ba8bc9e-36ba-43b9-891d-6d6c1be411c2
impl DaemonArgs {
    /// @id: 273997e9-805d-49a5-b5f0-b16729bcc1f3
    /// Resolves the database URL from the flag, then `DATABASE_URL`, then the default.
    pub fn database_url(&self) -> String {
        self.database_url
            .clone()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .unwrap_or_else(|| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string())
    }

    /// @id: ac380497-99b5-48e8-8700-e09735cdadd3
    /// Resolves the blob store path from the flag, then `SMARTFS_STORE_PATH`, then the default.
    pub fn store_path(&self) -> PathBuf {
        self.store_path
            .clone()
            .or_else(|| std::env::var("SMARTFS_STORE_PATH").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/var/lib/smartfs/blobs"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountpoint_is_required_without_crash_points() {
        assert!(DaemonArgs::try_parse_from(["smartfsd"]).is_err());
    }

    #[test]
    fn crash_points_alone_parses() {
        let a = DaemonArgs::try_parse_from(["smartfsd", "--crash-points"]).unwrap();
        assert!(a.crash_points);
        assert!(a.mountpoint.is_none());
    }

    #[test]
    fn defaults_match_the_cli() {
        let a = DaemonArgs::try_parse_from(["smartfsd", "--mountpoint", "/mnt/x"]).unwrap();
        assert!(a.foreground);
        assert!(!a.allow_other);
        assert!(!a.no_semantic);
        assert_eq!(a.log_level, "info");
        assert_eq!(a.mountpoint.unwrap(), PathBuf::from("/mnt/x"));
    }
}
