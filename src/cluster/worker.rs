use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::cluster::{grpc_client, outbox};
use crate::storage::StorageEngine;
use crate::tui::events::ServerEvent;

pub async fn run_replication_worker(
    db: SqlitePool,
    engine: StorageEngine,
    events: mpsc::UnboundedSender<ServerEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(1500));
    info!("outbox replication worker started");

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("outbox worker shutting down");
                    break;
                }
            }
        }
        if *shutdown.borrow() {
            break;
        }

        let jobs = match outbox::claim(&db, 50).await {
            Ok(j) => j,
            Err(err) => {
                warn!("claim outbox: {err:#}");
                continue;
            }
        };

        for job in jobs {
            debug!(
                "processing outbox id={} op={} peer={}",
                job.id, job.operation, job.peer_id
            );
            let result = process_job(&db, &engine, &job).await;
            match result {
                Ok(()) => {
                    if let Err(err) = outbox::complete(&db, job.id).await {
                        warn!("complete outbox {}: {err:#}", job.id);
                    } else {
                        let _ = events.send(ServerEvent::PeerConnected(job.peer_id.clone()));
                    }
                }
                Err(err) => {
                    warn!("outbox {} failed: {err:#}", job.id);
                    let _ = outbox::fail(&db, job.id).await;
                }
            }
        }
    }
}

async fn process_job(
    _db: &SqlitePool,
    engine: &StorageEngine,
    job: &crate::db::models::OutboxJob,
) -> anyhow::Result<()> {
    let mut client = grpc_client::connect(&job.wireguard_endpoint).await?;
    match job.operation.as_str() {
        "PUT" => {
            let _ = grpc_client::replicate_put(&mut client, engine, job).await?;
        }
        "DELETE" => {
            let _ = grpc_client::replicate_delete(&mut client, job).await?;
        }
        other => anyhow::bail!("unknown operation {other}"),
    }
    Ok(())
}

/// Convenience wrapper when wrapping engine in Arc for shared state.
#[allow(dead_code)]
pub async fn run_with_arc(
    db: SqlitePool,
    engine: Arc<StorageEngine>,
    events: mpsc::UnboundedSender<ServerEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    run_replication_worker(db, (*engine).clone(), events, shutdown).await;
}
