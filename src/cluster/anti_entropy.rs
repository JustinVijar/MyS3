use std::time::Duration;

use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::cluster::{grpc_client, outbox};
use crate::db::repository;

/// Hourly anti-entropy: for each active peer, SyncDigest and enqueue outbound PUTs
/// for objects the peer is missing. Reverse heal happens when the peer runs its own cron.
pub async fn run_anti_entropy(
    db: SqlitePool,
    local_node_id: String,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));
    // Also run once shortly after boot.
    let mut boot_delay = tokio::time::interval(Duration::from_secs(15));
    boot_delay.tick().await;

    info!("anti-entropy loop started");

    loop {
        tokio::select! {
            _ = boot_delay.tick() => {
                run_once(&db, &local_node_id).await;
                // disable further boot ticks by waiting on hourly only after first
                break;
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }

    loop {
        tokio::select! {
            _ = interval.tick() => {
                run_once(&db, &local_node_id).await;
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("anti-entropy shutting down");
                    break;
                }
            }
        }
    }
}

async fn run_once(db: &SqlitePool, local_node_id: &str) {
    let peers = match repository::list_active_peers(db).await {
        Ok(p) => p,
        Err(err) => {
            warn!("anti-entropy list peers: {err:#}");
            return;
        }
    };

    let local = match repository::digest_map(db, "").await {
        Ok(m) => m,
        Err(err) => {
            warn!("anti-entropy local digest: {err:#}");
            return;
        }
    };

    for peer in peers {
        if peer.id == local_node_id {
            continue;
        }
        let mut client = match grpc_client::connect(&peer.wireguard_endpoint).await {
            Ok(c) => c,
            Err(err) => {
                warn!(
                    "anti-entropy connect {}: {err:#}",
                    peer.wireguard_endpoint
                );
                continue;
            }
        };

        let remote = match grpc_client::sync_digest(&mut client, "").await {
            Ok(d) => d.object_etags,
            Err(err) => {
                warn!("anti-entropy SyncDigest {}: {err:#}", peer.id);
                continue;
            }
        };

        let mut enqueued = 0u32;
        for (filepath, etag) in &local {
            let missing = match remote.get(filepath) {
                None => true,
                Some(remote_etag) => remote_etag != etag,
            };
            if !missing {
                continue;
            }
            if let Ok(Some(obj)) = repository::object_by_filepath(db, filepath).await {
                let allowed = match repository::peer_should_receive_bucket(
                    db,
                    &peer.id,
                    obj.bucket_id,
                )
                .await
                {
                    Ok(v) => v,
                    Err(err) => {
                        warn!("anti-entropy peer filter {}: {err:#}", peer.id);
                        continue;
                    }
                };
                if !allowed {
                    continue;
                }
                if let Err(err) =
                    outbox::enqueue_put_peer(db, &peer.id, obj.id, &obj.filepath, &obj.etag).await
                {
                    warn!("anti-entropy enqueue {}: {err:#}", peer.id);
                } else {
                    enqueued += 1;
                }
            }
        }
        info!(
            "anti-entropy peer {}: enqueued {enqueued} PUT(s) (local={}, remote={})",
            peer.id,
            local.len(),
            remote.len()
        );
    }
}
