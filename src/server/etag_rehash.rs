use tracing::{error, info, warn};

use crate::db::models::EtagType;
use crate::db::repository;
use crate::AppState;

/// Background rehash of all active objects in a bucket to `etag_type`.
pub fn spawn_bucket_rehash(state: AppState, bucket_id: i64, etag_type: EtagType) {
    tokio::spawn(async move {
        if let Err(err) = run_bucket_rehash(state, bucket_id, etag_type).await {
            error!("bucket {bucket_id} etag rehash failed: {err:#}");
        }
    });
}

async fn run_bucket_rehash(
    state: AppState,
    bucket_id: i64,
    etag_type: EtagType,
) -> anyhow::Result<()> {
    let objects = repository::list_objects_for_etag_rehash(&state.db, bucket_id).await?;
    info!(
        "bucket {bucket_id} etag rehash starting: {} objects -> {} (folder markers skipped)",
        objects.len(),
        etag_type
    );

    let mut fatal: Option<String> = None;
    for obj in objects {
        match rehash_one(&state, &obj.filepath, obj.id, etag_type).await {
            Ok(()) => {
                if let Err(err) =
                    repository::bump_bucket_etag_rehash_processed(&state.db, bucket_id).await
                {
                    fatal = Some(err.to_string());
                    break;
                }
            }
            Err(err) => {
                warn!(
                    "bucket {bucket_id} skip object {} ({}): {err:#}",
                    obj.id, obj.original_filename
                );
                // Still count as processed so progress advances.
                let _ = repository::bump_bucket_etag_rehash_processed(&state.db, bucket_id).await;
            }
        }
    }

    if let Some(msg) = fatal {
        repository::finish_bucket_etag_rehash(&state.db, bucket_id, "error", Some(&msg)).await?;
        return Err(anyhow::anyhow!(msg));
    }

    repository::finish_bucket_etag_rehash(&state.db, bucket_id, "done", None).await?;
    info!("bucket {bucket_id} etag rehash finished");
    Ok(())
}

async fn rehash_one(
    state: &AppState,
    filepath: &str,
    object_id: i64,
    etag_type: EtagType,
) -> anyhow::Result<()> {
    let etag = state.engine.hash_filepath(filepath, etag_type).await?;
    repository::update_object_etag(&state.db, object_id, etag_type, &etag).await?;

    let mut tx = state.db.begin().await?;
    repository::enqueue_put_for_active_peers(
        &mut tx,
        object_id,
        filepath,
        &etag,
        &state.config.node_id,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
