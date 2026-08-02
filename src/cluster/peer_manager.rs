use anyhow::Result;
use sqlx::SqlitePool;
use tracing::info;

use crate::config::Config;
use crate::db::models::QuotaMode;
use crate::db::repository::{self, DEFAULT_NODE_ALLOCATED_BYTES};

pub async fn seed_peers_from_config(pool: &SqlitePool, config: &Config) -> Result<()> {
    for peer in &config.cluster_peers {
        repository::upsert_peer(pool, &peer.id, &peer.endpoint).await?;
        info!("seeded cluster peer {} -> {}", peer.id, peer.endpoint);
    }
    Ok(())
}

/// Register the local node in `cluster_peer` and ensure every bucket assigns it.
pub async fn ensure_local_node(pool: &SqlitePool, config: &Config) -> Result<()> {
    let endpoint = config.grpc_bind_addr.to_string();
    repository::upsert_peer(pool, &config.node_id, &endpoint).await?;
    info!(
        "ensured local cluster peer {} -> {}",
        config.node_id, endpoint
    );

    let bucket_ids = repository::list_bucket_ids(pool).await?;
    for bucket_id in bucket_ids {
        repository::ensure_bucket_node_assignment(
            pool,
            bucket_id,
            &config.node_id,
            DEFAULT_NODE_ALLOCATED_BYTES,
            QuotaMode::Soft,
        )
        .await?;
    }
    // Prefer explicit assignments going forward for buckets that only had replicate_to_all.
    // Do not flip replicate_to_all here for buckets that still use "all peers" with remotes;
    // backfill only adds the local row. New node API edits clear replicate_to_all.
    Ok(())
}

pub async fn note_heartbeat(pool: &SqlitePool, node_id: &str) -> Result<()> {
    repository::update_peer_heartbeat(pool, node_id, chrono::Utc::now()).await
}
