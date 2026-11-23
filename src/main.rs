mod auth;
mod config;
mod db;
mod hls;
mod ingest;
mod server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::run().await?;
    Ok(())
}
