use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use sqlx::SqlitePool;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::cluster::outbox;
use crate::db::rbac;
use crate::db::repository;
use crate::storage::StorageEngine;

/// Periodically hard-delete recycle-bin objects past the configured retention.
pub async fn run_recycle_purge_worker(
    db: SqlitePool,
    engine: StorageEngine,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    info!("recycle-bin purge worker started");

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("recycle-bin purge worker shutting down");
                    break;
                }
            }
        }
        if *shutdown.borrow() {
            break;
        }

        if let Err(err) = purge_once(&db, &engine).await {
            warn!("recycle purge tick failed: {err:#}");
        }
    }
}

async fn purge_once(db: &SqlitePool, engine: &StorageEngine) -> anyhow::Result<()> {
    let settings = rbac::get_settings(db).await?;
    let secs = settings
        .recycle_retention_unit
        .to_seconds(settings.recycle_retention_value);
    // Retention 0 → purge immediately (anything already soft-deleted).
    let cutoff = Utc::now() - ChronoDuration::seconds(secs.max(0));

    let expired = repository::list_expired_deleted_objects(db, cutoff).await?;
    for obj in expired {
        let mut tx = db.begin().await?;
        outbox::enqueue_delete_tx(&mut tx, obj.id, &obj.filepath, &obj.etag).await?;
        sqlx::query(r#"DELETE FROM object WHERE id = ?1"#)
            .bind(obj.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if let Err(err) = engine.unlink(&obj.filepath).await {
            warn!("unlink purged object {}: {err:#}", obj.filepath);
        } else {
            info!(
                "purged recycled object id={} key={}",
                obj.id, obj.original_filename
            );
        }
    }
    Ok(())
}
