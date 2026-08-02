//! Shared outbox helpers used by S3 routes and anti-entropy.

use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::db::repository;

pub async fn enqueue_put_tx(
    tx: &mut Transaction<'_, Sqlite>,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
    local_node_id: &str,
) -> Result<u64> {
    repository::enqueue_put_for_active_peers(tx, object_id, filepath_uuid, etag, local_node_id)
        .await
}

pub async fn enqueue_delete_tx(
    tx: &mut Transaction<'_, Sqlite>,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
    local_node_id: &str,
) -> Result<u64> {
    repository::enqueue_delete_for_active_peers(
        tx,
        object_id,
        filepath_uuid,
        etag,
        local_node_id,
    )
    .await
}

pub async fn enqueue_put_peer(
    pool: &SqlitePool,
    peer_id: &str,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
) -> Result<()> {
    repository::enqueue_put_for_peer(pool, peer_id, object_id, filepath_uuid, etag).await
}

pub async fn claim(pool: &SqlitePool, limit: i64) -> Result<Vec<crate::db::models::OutboxJob>> {
    repository::claim_outbox_jobs(pool, limit).await
}

pub async fn complete(pool: &SqlitePool, id: i64) -> Result<()> {
    repository::complete_outbox_job(pool, id).await
}

pub async fn fail(pool: &SqlitePool, id: i64) -> Result<()> {
    repository::fail_outbox_job(pool, id).await
}
