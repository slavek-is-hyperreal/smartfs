//! smartfs-mcp — Model Context Protocol stdio server binary.

use std::path::PathBuf;
use std::sync::Arc;

use smartfs_mcp::handler::ToolHandler;
use smartfs_mcp::server::McpServer;
use smartfs_store::LocalDiskStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    let db_url = std::env::var("SMARTFS_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://postgres:postgres@172.17.0.2:5432/smartfs".to_string());

    let store_path = std::env::var("SMARTFS_STORE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/smartfs/blobs"));

    let pool = smartfs_db::connect_pool(&db_url).await?;
    let store = Arc::new(LocalDiskStore::new(&store_path));

    let handler = ToolHandler::new(pool, store, None);
    let server = McpServer::new(handler);

    server.run_stdio().await?;
    Ok(())
}
