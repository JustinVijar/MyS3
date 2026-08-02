use anyhow::Result;
use sqlx::SqlitePool;
use tracing::info;

use crate::config::Config;
use crate::db::repository;

pub async fn seed_peers_from_config(pool: &SqlitePool, config: &Config) -> Result<()> {
    for peer in &config.cluster_peers {
        repository::upsert_peer(pool, &peer.id, &peer.endpoint).await?;
        info!("seeded cluster peer {} -> {}", peer.id, peer.endpoint);
    }
    Ok(())
}

pub async fn note_heartbeat(pool: &SqlitePool, node_id: &str) -> Result<()> {
    repository::update_peer_heartbeat(pool, node_id, chrono::Utc::now()).await
}
