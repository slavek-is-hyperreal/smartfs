//! `smartfsd` — the SmartFS FUSE daemon entrypoint.
//!
//! Implements the startup sequence of docs/testing/the-great-smartfs-test.md
//! §1.3 in exactly the order given there. Every numbered step is a hard gate:
//! on failure the error chain is logged at `ERROR`, the process does not
//! proceed, does not fall back to a degraded mode, and exits with the code
//! §1.3 assigns. Step 9 (the consolidation supervisors) is the one deliberate
//! exception — Root Invariant #6 says that layer never blocks the live path,
//! and that has to include never blocking startup.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use fuser::{BackgroundSession, MountOption};
use smartfs_daemon::args::DaemonArgs;
use smartfs_daemon::exit;
use smartfs_daemon::startup;
use smartfs_fuse::{spawn_mount_smartfs, PendingLimits, PendingPipeline, SmartFsFuse};
use smartfs_store::BlobStore;
use tokio::signal::unix::{signal, SignalKind};
use tokio::task::JoinHandle;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter};

/// How long step 10 waits for the kernel to show the mount as a live FUSE
/// filesystem before declaring the mount failed.
const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a clean shutdown waits for the ADR-58 pending queue to drain.
/// Exceeding it is safe: unfinished markers are durable and replay on restart.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Gap between checksum scrub passes (ADR-58 decision point 7). Deliberately
/// long: this hunts bit rot, which is slow, and must never crowd out live I/O.
const SCRUB_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Blobs verified per scrub pass. A sample, so one pass is bounded however
/// large the store grows; successive passes cover it over time.
const SCRUB_SAMPLE: i64 = 512;

/// Crash-point labels compiled into this binary (§1.5).
///
/// Stage 0b — the `crash_point!` macro behind `smartfs-db`'s non-default
/// `crash-test` feature — is deliberately not implemented, so the list is
/// empty and `--crash-points` exits 1. Stage 4's script reads that as
/// "uninstrumented binary" and refuses deterministic mode, which is the
/// intended loud behaviour rather than a silent degradation to random kills.
const COMPILED_CRASH_POINTS: &[&str] = &[];

/// A gate failure carrying the §1.3 exit code that goes with it.
struct Fatal {
    code: i32,
    err: anyhow::Error,
}

/// Attaches an exit code to a gate result.
trait GateExt<T> {
    fn gate(self, code: i32) -> std::result::Result<T, Fatal>;
}

/// @id: c42e9d89-3666-4059-8879-943d064bdcbe
impl<T> GateExt<T> for Result<T> {
    fn gate(self, code: i32) -> std::result::Result<T, Fatal> {
        self.map_err(|err| Fatal { code, err })
    }
}

/// What step 9 concluded about the consolidation layer, as recorded in the
/// ready file (§1.4).
enum Semantic {
    /// Supervisors requested and started; carries how many are running.
    Ok(usize),
    /// Supervisors requested but could not be started. The filesystem serves anyway.
    Degraded,
    /// `--no-semantic`.
    Off,
}

/// @id: bae59b34-bdb3-4e9e-a745-f88b45ba3cef
impl Semantic {
    fn as_str(&self) -> &'static str {
        match self {
            Semantic::Ok(_) => "ok",
            Semantic::Degraded => "degraded",
            Semantic::Off => "off",
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // ── Step 1. tracing, before anything that can fail ──────────────────────
    // RUST_LOG wins when set; otherwise the subscriber starts at `info` and is
    // reloaded to --log-level once step 2 has parsed it.
    let rust_log = std::env::var("RUST_LOG").ok();
    let initial = rust_log
        .as_deref()
        .map(EnvFilter::new)
        .unwrap_or_else(|| EnvFilter::new("info"));
    let (filter, filter_handle) = reload::Layer::new(initial);
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();

    // ── Step 2. args ────────────────────────────────────────────────────────
    // Parsed by hand rather than via `parse()` so a usage error does not exit
    // with clap's default code 2 — §1.3 reserves 2 for an invalid mountpoint
    // and the scripts rely on the codes being distinguishable.
    let args = match DaemonArgs::try_parse() {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            // `--help` and `--version` come back as errors but are a success.
            std::process::exit(match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
                _ => 1,
            });
        }
    };

    if rust_log.is_none() {
        match EnvFilter::try_new(&args.log_level) {
            Ok(f) => {
                let _ = filter_handle.modify(|slot| *slot = f);
            }
            Err(e) => {
                tracing::error!("invalid --log-level {:?}: {e}; staying at 'info'", args.log_level);
            }
        }
    }

    // `--crash-points` answers before any gate, so Stage 4 can interrogate a
    // binary without a mountpoint, a database or a store.
    if args.crash_points {
        for label in COMPILED_CRASH_POINTS {
            println!("{label}");
        }
        std::process::exit(if COMPILED_CRASH_POINTS.is_empty() { 1 } else { 0 });
    }

    match run(args).await {
        Ok(()) => std::process::exit(exit::SUCCESS),
        Err(Fatal { code, err }) => {
            tracing::error!("startup failed: {err:#}");
            for (i, cause) in err.chain().skip(1).enumerate() {
                tracing::error!("  cause {}: {cause}", i + 1);
            }
            tracing::error!("exiting with code {code}");
            std::process::exit(code);
        }
    }
}

/// Runs steps 3 through 11. Returns only on a clean signal-driven shutdown.
async fn run(args: DaemonArgs) -> std::result::Result<(), Fatal> {
    // ── Step 3. mountpoint ──────────────────────────────────────────────────
    let requested = args
        .mountpoint
        .clone()
        .ok_or_else(|| anyhow!("--mountpoint is required"))
        .gate(exit::MOUNTPOINT_INVALID)?;
    let mountpoint = startup::validate_mountpoint(&requested).gate(exit::MOUNTPOINT_INVALID)?;
    tracing::info!(mountpoint = %mountpoint.display(), "mountpoint validated");

    // ── Step 4. store path ──────────────────────────────────────────────────
    let store_path = startup::validate_store_path(&args.store_path()).gate(exit::STORE_PATH_INVALID)?;
    tracing::info!(store_path = %store_path.display(), "blob store path validated");

    // ── Step 5. database pool + SELECT 1 ────────────────────────────────────
    let database_url = args.database_url();
    let pool = startup::connect_and_probe_db(&database_url)
        .await
        .gate(exit::DB_CONNECT_FAILED)?;
    tracing::info!("database connected and answering SELECT 1");

    // ── Step 6. schema sanity ───────────────────────────────────────────────
    startup::check_schema(&pool).await.gate(exit::SCHEMA_CHECK_FAILED)?;

    // ── Step 7. blob store handle ───────────────────────────────────────────
    let store = startup::open_store(&store_path).await.gate(exit::STORE_OPEN_FAILED)?;
    let store: Arc<dyn BlobStore + Send + Sync> = Arc::new(store);
    tracing::info!("blob store handle opened");

    // ── Step 8. FUSE state + background mount ───────────────────────────────
    let mut options = vec![
        MountOption::DefaultPermissions,
        MountOption::FSName("smartfs".to_string()),
        MountOption::AutoUnmount,
    ];
    if args.allow_other {
        options.push(MountOption::AllowOther);
    }

    // ADR-58: the two-stage commit pipeline. Started before the mount so no
    // write can ever reach a filesystem whose drain is not running.
    let limits = PendingLimits::from_env();
    let pipeline = PendingPipeline::start(
        pool.clone(),
        &store_path,
        &tokio::runtime::Handle::current(),
        limits,
    )
    .await
    .map_err(|e| anyhow!("{e}"))
    .context("cannot start the pending-commit pipeline")
    .gate(exit::STORE_OPEN_FAILED)?;

    let fs = SmartFsFuse::new(
        pool.clone(),
        Arc::clone(&store),
        &store_path,
        tokio::runtime::Handle::current(),
        false,
        pipeline.clone(),
    );

    // Held for the whole process lifetime: dropping it unmounts.
    let session: BackgroundSession = spawn_mount_smartfs(fs, &mountpoint, &options)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("spawn_mount_smartfs({}) failed", mountpoint.display()))
        .gate(exit::MOUNT_FAILED)?;
    tracing::info!(
        allow_other = args.allow_other,
        "FUSE session spawned; waiting for the kernel to serve the mount"
    );

    // ── Step 9. consolidation supervisors (the one non-fatal step) ──────────
    let mut supervisors: Vec<JoinHandle<()>> = Vec::new();
    let semantic = if args.no_semantic {
        tracing::info!("--no-semantic: consolidation supervisors not started");
        Semantic::Off
    } else {
        match smartfs_semantic::fetch_calibrated_combinations(&pool).await {
            Ok(combos) => {
                // Zero rows in consolidation_thresholds means zero supervisors
                // by design (fail-safe, not fail-open). Log the count so it is
                // never mistaken for a crash.
                tracing::info!(
                    count = combos.len(),
                    "starting consolidation supervisors, one per (plugin_type, model_id)"
                );
                for combo in combos {
                    let db = pool.clone();
                    supervisors.push(tokio::spawn(async move {
                        smartfs_semantic::consolidation_supervisor(db, combo).await;
                    }));
                }
                Semantic::Ok(supervisors.len())
            }
            Err(e) => {
                tracing::error!(
                    "consolidation supervisors could not be started: {e}. \
                     Continuing to serve the filesystem — Root Invariant #6 says this \
                     layer never blocks the live path, and that includes startup."
                );
                Semantic::Degraded
            }
        }
    };

    // Checksum scrub (ADR-58 decision point 7). Distinct from the pending
    // scan in every way that matters: that one looks for a database row
    // missing for a file that exists, this one for a file gone bad under a row
    // that exists. Its logs say "scrub" so the two can never be confused.
    let scrub_pool = pool.clone();
    let scrub_root = store_path.clone();
    let scrub_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(SCRUB_INTERVAL).await;
            match smartfs_db::list_blob_digests(&scrub_pool, Some(SCRUB_SAMPLE)).await {
                Ok(expectations) if expectations.is_empty() => {}
                Ok(expectations) => {
                    let report = smartfs_store::scrub_once(
                        &scrub_root,
                        &expectations,
                        smartfs_store::ScrubScope::Full,
                        smartfs_store::ScrubPacing::default(),
                        |bytes| {
                            smartfs_compress::decompress(bytes)
                                .map(|plain| smartfs_compress::hash_bytes(&plain).0)
                                .map_err(|e| e.to_string())
                        },
                    )
                    .await;
                    match report {
                        Ok(r) if r.is_clean() => tracing::info!("{}", r.summary()),
                        Ok(r) => {
                            tracing::error!("{}", r.summary());
                            for f in &r.findings {
                                tracing::error!(
                                    blob = %f.blob_id,
                                    expected = %f.expected,
                                    "scrub found a damaged blob at {}: {:?}",
                                    f.path.display(),
                                    f.actual
                                );
                            }
                        }
                        Err(e) => tracing::error!("scrub pass failed: {e}"),
                    }
                }
                Err(e) => tracing::error!("scrub could not read blob digests: {e}"),
            }
        }
    });

    // ── Step 10. prove the mount is serving, then publish readiness ─────────
    let fstype = wait_until_serving(&mountpoint, MOUNT_READY_TIMEOUT)
        .await
        .gate(exit::MOUNT_FAILED)?;
    tracing::info!(fstype = %fstype, "mount is live and serving");

    // ADR-58: Ensure any replayed crash markers finish committing before advertising readiness.
    if !pipeline.quiesce(std::time::Duration::from_secs(5)).await {
        tracing::warn!("quiesce after startup replay timed out after 5s");
    }

    let pid = std::process::id();
    if let Some(path) = args.pid_file.as_deref() {
        write_atomic(path, &format!("{pid}\n"))
            .with_context(|| format!("cannot write pid file {}", path.display()))
            .gate(exit::MOUNT_FAILED)?;
    }
    if let Some(path) = args.ready_file.as_deref() {
        let body = format!(
            "pid={pid}\nmountpoint={}\nsemantic={}\nsupervisors={}\nstore_path={}\nfstype={fstype}\nallow_other={}\n",
            mountpoint.display(),
            semantic.as_str(),
            match semantic {
                Semantic::Ok(n) => n,
                _ => 0,
            },
            store_path.display(),
            args.allow_other,
        );
        write_atomic(path, &body)
            .with_context(|| format!("cannot write ready file {}", path.display()))
            .gate(exit::MOUNT_FAILED)?;
    }
    tracing::info!(
        pid,
        semantic = semantic.as_str(),
        "smartfsd ready at {}",
        mountpoint.display()
    );

    // ── Step 11. signals ────────────────────────────────────────────────────
    // SIGKILL is intentionally unhandled: it is Stage 4's instrument.
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| anyhow!("cannot install SIGINT handler: {e}"))
        .gate(exit::MOUNT_FAILED)?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| anyhow!("cannot install SIGTERM handler: {e}"))
        .gate(exit::MOUNT_FAILED)?;

    tokio::select! {
        _ = sigint.recv()  => tracing::info!("SIGINT received, shutting down"),
        _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
    }

    scrub_task.abort();
    for handle in &supervisors {
        handle.abort();
    }
    tracing::info!(count = supervisors.len(), "consolidation supervisors stopped");

    // Give the pending drain a chance to land queued writes before we go
    // (ADR-58). Failing to finish is not data loss — the markers stay on disk
    // and the next start replays them — so this never blocks shutdown for long.
    let queued = pipeline.ram_depth();
    if queued > 0 {
        tracing::info!(queued, "draining pending queue before unmount");
        if pipeline.quiesce(SHUTDOWN_DRAIN_TIMEOUT).await {
            tracing::info!("pending queue drained");
        } else {
            tracing::warn!(
                remaining = pipeline.ram_depth(),
                "pending queue not drained within the shutdown budget; \
                 markers remain on disk and will be replayed on next start"
            );
        }
    }

    drop(session); // unmounts
    tracing::info!("FUSE session dropped, {} unmounted", mountpoint.display());

    for path in [args.pid_file.as_deref(), args.ready_file.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Err(e) = fs::remove_file(path) {
            tracing::warn!("could not remove {}: {e}", path.display());
        }
    }

    pool.close().await;
    Ok(())
}

/// Step 10's gate: poll until the mountpoint reports a live FUSE filesystem.
///
/// Both halves have to hold — `/proc/self/mountinfo` must show a `fuse*`
/// filesystem at the path, and a `stat()` of the path must succeed, which only
/// happens once the daemon is answering `getattr` for the root inode.
async fn wait_until_serving(mountpoint: &Path, timeout: Duration) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let last: String;
        match startup::mountinfo_fstype(mountpoint)? {
            Some(fstype) if fstype.starts_with("fuse") => match fs::metadata(mountpoint) {
                Ok(meta) if meta.is_dir() => return Ok(fstype),
                Ok(_) => last = format!("{fstype} mounted but the root is not a directory"),
                Err(e) => last = format!("{fstype} mounted but stat() failed: {e}"),
            },
            Some(other) => last = format!("mounted, but as '{other}', not a FUSE filesystem"),
            None => last = "not a mount".to_string(),
        }

        if std::time::Instant::now() >= deadline {
            bail!(
                "{} did not become a serving FUSE mount within {:?}: {last}",
                mountpoint.display(),
                timeout
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Writes `body` to `path` via a temporary file and a rename, so a reader
/// polling the path never observes a half-written file.
fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, body).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("cannot rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}
