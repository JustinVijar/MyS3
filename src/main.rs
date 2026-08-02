mod cluster;
mod config;
mod db;
mod network;
mod server;
mod storage;
mod tui;

use std::sync::Arc;

use anyhow::Context;
use sqlx::SqlitePool;
use tokio::sync::{mpsc, watch};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::cluster::grpc_server::NodeReplicationService;
use crate::config::Config;
use crate::network::wireguard::WireGuardRuntime;
use crate::storage::StorageEngine;
use crate::tui::events::ServerEvent;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub engine: StorageEngine,
    pub events: mpsc::UnboundedSender<ServerEvent>,
    pub wg: Arc<WireGuardRuntime>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env().context("load config")?;
    let config = Arc::new(config);

    let engine = StorageEngine::init(config.storage_root.clone())
        .await
        .context("init storage")?;
    let db = db::repository::connect_and_migrate(&config.metadata_db_path())
        .await
        .context("migrate db")?;

    cluster::peer_manager::seed_peers_from_config(&db, &config).await?;

    let wg = Arc::new(WireGuardRuntime::start(&config)?);
    let (events_tx, events_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state = AppState {
        config: config.clone(),
        db: db.clone(),
        engine: engine.clone(),
        events: events_tx.clone(),
        wg: wg.clone(),
    };

    let http_addr = wg.http_bind(&config);
    let grpc_addr = wg.grpc_bind(&config);

    // HTTP (S3 + web)
    let app = server::build_router(state.clone());
    let http_shutdown = shutdown_rx.clone();
    let http_handle = tokio::spawn(async move {
        info!("HTTP listening on {http_addr}");
        let server = axum_server::bind(http_addr).serve(app.into_make_service());
        tokio::select! {
            res = server => {
                if let Err(err) = res {
                    error!("HTTP server error: {err:#}");
                }
            }
            _ = wait_shutdown(http_shutdown) => {
                info!("HTTP server shutting down");
            }
        }
    });

    // gRPC replication
    let grpc_service = NodeReplicationService {
        db: db.clone(),
        engine: engine.clone(),
        node_id: config.node_id.clone(),
        events: events_tx.clone(),
    };
    let grpc_shutdown = shutdown_rx.clone();
    let grpc_handle = tokio::spawn(async move {
        if let Err(err) = cluster::grpc_server::serve(grpc_addr, grpc_service, grpc_shutdown).await
        {
            error!("gRPC server error: {err:#}");
        }
    });

    // Outbox worker
    let worker_db = db.clone();
    let worker_engine = engine.clone();
    let worker_events = events_tx.clone();
    let worker_shutdown = shutdown_rx.clone();
    let worker_handle = tokio::spawn(async move {
        cluster::worker::run_replication_worker(
            worker_db,
            worker_engine,
            worker_events,
            worker_shutdown,
        )
        .await;
    });

    // Anti-entropy
    let ae_db = db.clone();
    let ae_shutdown = shutdown_rx.clone();
    let ae_handle = tokio::spawn(async move {
        cluster::anti_entropy::run_anti_entropy(ae_db, ae_shutdown).await;
    });

    // Recycle-bin retention purge
    let purge_db = db.clone();
    let purge_engine = engine.clone();
    let purge_shutdown = shutdown_rx.clone();
    let purge_handle = tokio::spawn(async move {
        server::purge::run_recycle_purge_worker(purge_db, purge_engine, purge_shutdown).await;
    });

    // Ctrl+C → shutdown
    let shutdown_tx_ctrl = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Ctrl+C received");
        let _ = shutdown_tx_ctrl.send(true);
    });

    if config.disable_tui {
        info!("TUI disabled; running headless until shutdown");
        wait_shutdown(shutdown_rx).await;
    } else {
        // TUI on a blocking-friendly path; it polls crossterm + drains events.
        let tui_engine = engine.clone();
        if let Err(err) = tui::run(events_rx, tui_engine, shutdown_tx).await {
            error!("TUI error: {err:#}");
            // fall through to wait for shutdown signal if TUI failed early
            wait_shutdown(shutdown_rx).await;
        }
    }

    let _ = http_handle.await;
    let _ = grpc_handle.await;
    let _ = worker_handle.await;
    let _ = ae_handle.await;
    let _ = purge_handle.await;
    info!("shutdown complete");
    Ok(())
}

async fn wait_shutdown(mut rx: watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
}
