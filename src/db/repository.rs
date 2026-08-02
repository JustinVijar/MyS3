use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::models::{
    BucketNodeAssignment, ClusterPeer, EtagType, ObjectRecord, OutboxJob, QuotaMode,
};

pub const DEFAULT_NODE_ALLOCATED_BYTES: i64 = 100 * 1024 * 1024 * 1024; // 100 GiB

pub async fn connect_and_migrate(db_path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("connect sqlite {}", db_path.display()))?;

    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub async fn insert_object_tx(
    tx: &mut Transaction<'_, Sqlite>,
    original_filename: &str,
    filepath: &str,
    file_format: &str,
    filesize_bytes: i64,
    etag_type: &EtagType,
    etag: &str,
    bucket_id: i64,
) -> Result<i64> {
    let etag_type_s = etag_type.to_string();
    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO object (
            original_filename, filepath, file_format, filesize_bytes,
            etag_type, etag, bucket_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        RETURNING id
        "#,
    )
    .bind(original_filename)
    .bind(filepath)
    .bind(file_format)
    .bind(filesize_bytes)
    .bind(etag_type_s)
    .bind(etag)
    .bind(bucket_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Enqueue PUT jobs for peers configured on the object's bucket.
/// `exclude_peer_id` is the local node id (never replicate to self).
pub async fn enqueue_put_for_active_peers(
    tx: &mut Transaction<'_, Sqlite>,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
    exclude_peer_id: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO replication_outbox (peer_id, object_id, filepath_uuid, etag, operation)
        SELECT p.id, ?1, ?2, ?3, 'PUT'
        FROM cluster_peer p
        JOIN object o ON o.id = ?1
        JOIN bucket b ON b.id = o.bucket_id
        WHERE p.is_active = 1
          AND p.id != ?4
          AND (
            b.replicate_to_all = 1
            OR EXISTS (
                SELECT 1 FROM bucket_replication_peer brp
                WHERE brp.bucket_id = b.id AND brp.peer_id = p.id
            )
          )
        "#,
    )
    .bind(object_id)
    .bind(filepath_uuid)
    .bind(etag)
    .bind(exclude_peer_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// Enqueue DELETE jobs for peers configured on the object's bucket.
pub async fn enqueue_delete_for_active_peers(
    tx: &mut Transaction<'_, Sqlite>,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
    exclude_peer_id: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO replication_outbox (peer_id, object_id, filepath_uuid, etag, operation)
        SELECT p.id, ?1, ?2, ?3, 'DELETE'
        FROM cluster_peer p
        JOIN object o ON o.id = ?1
        JOIN bucket b ON b.id = o.bucket_id
        WHERE p.is_active = 1
          AND p.id != ?4
          AND (
            b.replicate_to_all = 1
            OR EXISTS (
                SELECT 1 FROM bucket_replication_peer brp
                WHERE brp.bucket_id = b.id AND brp.peer_id = p.id
            )
          )
        "#,
    )
    .bind(object_id)
    .bind(filepath_uuid)
    .bind(etag)
    .bind(exclude_peer_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// True if `peer_id` should receive objects from `bucket_id`.
pub async fn peer_should_receive_bucket(
    pool: &SqlitePool,
    peer_id: &str,
    bucket_id: i64,
) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT 1
        FROM bucket b
        WHERE b.id = ?1
          AND (
            b.replicate_to_all = 1
            OR EXISTS (
                SELECT 1 FROM bucket_replication_peer brp
                WHERE brp.bucket_id = b.id AND brp.peer_id = ?2
            )
          )
        "#,
    )
    .bind(bucket_id)
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

pub async fn get_bucket_replication(
    pool: &SqlitePool,
    bucket_id: i64,
) -> Result<(bool, Vec<String>)> {
    let replicate_to_all: Option<bool> = sqlx::query_scalar(
        r#"SELECT replicate_to_all FROM bucket WHERE id = ?1"#,
    )
    .bind(bucket_id)
    .fetch_optional(pool)
    .await?;
    let Some(replicate_to_all) = replicate_to_all else {
        anyhow::bail!("bucket not found");
    };
    let peer_ids = sqlx::query_scalar::<_, String>(
        r#"SELECT peer_id FROM bucket_replication_peer WHERE bucket_id = ?1 ORDER BY peer_id ASC"#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    Ok((replicate_to_all, peer_ids))
}

pub async fn set_bucket_replication(
    pool: &SqlitePool,
    bucket_id: i64,
    replicate_to_all: bool,
    peer_ids: &[String],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        r#"UPDATE bucket SET replicate_to_all = ?1 WHERE id = ?2"#,
    )
    .bind(replicate_to_all)
    .bind(bucket_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        anyhow::bail!("bucket not found");
    }
    sqlx::query(r#"DELETE FROM bucket_replication_peer WHERE bucket_id = ?1"#)
        .bind(bucket_id)
        .execute(&mut *tx)
        .await?;
    if !replicate_to_all {
        for peer_id in peer_ids {
            // Skip unknown/inactive peers silently if not present.
            let exists: Option<i64> = sqlx::query_scalar(
                r#"SELECT 1 FROM cluster_peer WHERE id = ?1 AND is_active = 1"#,
            )
            .bind(peer_id)
            .fetch_optional(&mut *tx)
            .await?;
            if exists.is_none() {
                continue;
            }
            sqlx::query(
                r#"
                INSERT OR IGNORE INTO bucket_replication_peer (
                    bucket_id, peer_id, allocated_bytes, quota_mode
                )
                VALUES (?1, ?2, ?3, 'soft')
                "#,
            )
            .bind(bucket_id)
            .bind(peer_id)
            .bind(DEFAULT_NODE_ALLOCATED_BYTES)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list_bucket_ids(pool: &SqlitePool) -> Result<Vec<i64>> {
    let ids = sqlx::query_scalar::<_, i64>(r#"SELECT id FROM bucket ORDER BY id ASC"#)
        .fetch_all(pool)
        .await?;
    Ok(ids)
}

pub async fn list_bucket_node_assignments(
    pool: &SqlitePool,
    bucket_id: i64,
) -> Result<Vec<BucketNodeAssignment>> {
    let rows = sqlx::query_as::<_, BucketNodeAssignment>(
        r#"
        SELECT bucket_id, peer_id, allocated_bytes, quota_mode
        FROM bucket_replication_peer
        WHERE bucket_id = ?1
        ORDER BY peer_id ASC
        "#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn bucket_used_bytes(pool: &SqlitePool, bucket_id: i64) -> Result<i64> {
    let bytes: i64 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(SUM(filesize_bytes), 0) FROM object
        WHERE bucket_id = ?1 AND deleted_at IS NULL
        "#,
    )
    .bind(bucket_id)
    .fetch_one(pool)
    .await?;
    Ok(bytes)
}

pub async fn ensure_bucket_node_assignment(
    pool: &SqlitePool,
    bucket_id: i64,
    peer_id: &str,
    allocated_bytes: i64,
    quota_mode: QuotaMode,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO bucket_replication_peer (
            bucket_id, peer_id, allocated_bytes, quota_mode
        )
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(bucket_id, peer_id) DO NOTHING
        "#,
    )
    .bind(bucket_id)
    .bind(peer_id)
    .bind(allocated_bytes)
    .bind(quota_mode)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn assign_bucket_node(
    pool: &SqlitePool,
    bucket_id: i64,
    peer_id: &str,
    allocated_bytes: i64,
    quota_mode: QuotaMode,
) -> Result<()> {
    if allocated_bytes <= 0 {
        bail!("allocated_bytes must be positive");
    }
    let peer_ok: Option<i64> = sqlx::query_scalar(
        r#"SELECT 1 FROM cluster_peer WHERE id = ?1 AND is_active = 1"#,
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    if peer_ok.is_none() {
        bail!("peer not found or inactive");
    }
    let mut tx = pool.begin().await?;
    sqlx::query(r#"UPDATE bucket SET replicate_to_all = 0 WHERE id = ?1"#)
        .bind(bucket_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query(
        r#"
        INSERT INTO bucket_replication_peer (
            bucket_id, peer_id, allocated_bytes, quota_mode
        )
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(bucket_id, peer_id) DO UPDATE SET
            allocated_bytes = excluded.allocated_bytes,
            quota_mode = excluded.quota_mode
        "#,
    )
    .bind(bucket_id)
    .bind(peer_id)
    .bind(allocated_bytes)
    .bind(quota_mode)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        // unreachable with upsert, keep for clarity
    }
    let _ = result;
    tx.commit().await?;
    Ok(())
}

pub async fn update_bucket_node(
    pool: &SqlitePool,
    bucket_id: i64,
    peer_id: &str,
    allocated_bytes: Option<i64>,
    quota_mode: Option<QuotaMode>,
) -> Result<()> {
    if allocated_bytes.is_none() && quota_mode.is_none() {
        bail!("no fields to update");
    }
    if let Some(b) = allocated_bytes {
        if b <= 0 {
            bail!("allocated_bytes must be positive");
        }
    }
    let existing = sqlx::query_as::<_, BucketNodeAssignment>(
        r#"
        SELECT bucket_id, peer_id, allocated_bytes, quota_mode
        FROM bucket_replication_peer
        WHERE bucket_id = ?1 AND peer_id = ?2
        "#,
    )
    .bind(bucket_id)
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = existing else {
        bail!("node assignment not found");
    };
    let bytes = allocated_bytes.unwrap_or(row.allocated_bytes);
    let mode = quota_mode.unwrap_or(row.quota_mode);
    let mut tx = pool.begin().await?;
    sqlx::query(r#"UPDATE bucket SET replicate_to_all = 0 WHERE id = ?1"#)
        .bind(bucket_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        UPDATE bucket_replication_peer
        SET allocated_bytes = ?1, quota_mode = ?2
        WHERE bucket_id = ?3 AND peer_id = ?4
        "#,
    )
    .bind(bytes)
    .bind(mode)
    .bind(bucket_id)
    .bind(peer_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_bucket_node(
    pool: &SqlitePool,
    bucket_id: i64,
    peer_id: &str,
    local_node_id: &str,
) -> Result<()> {
    if peer_id == local_node_id {
        bail!("cannot remove the local node");
    }
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM bucket_replication_peer WHERE bucket_id = ?1"#,
    )
    .bind(bucket_id)
    .fetch_one(pool)
    .await?;
    if count <= 1 {
        bail!("cannot remove the last node");
    }
    let result = sqlx::query(
        r#"DELETE FROM bucket_replication_peer WHERE bucket_id = ?1 AND peer_id = ?2"#,
    )
    .bind(bucket_id)
    .bind(peer_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("node assignment not found");
    }
    sqlx::query(r#"UPDATE bucket SET replicate_to_all = 0 WHERE id = ?1"#)
        .bind(bucket_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Returns Ok(()) if upload of `incoming_bytes` (net growth) is allowed under hard quotas.
/// Soft quotas never block. `net_growth` = new_size - replaced_size (for overwrite).
pub async fn check_hard_quota(
    pool: &SqlitePool,
    bucket_id: i64,
    net_growth: i64,
) -> Result<()> {
    if net_growth <= 0 {
        return Ok(());
    }
    let used = bucket_used_bytes(pool, bucket_id).await?;
    let projected = used.saturating_add(net_growth);
    let hard_rows = sqlx::query_as::<_, BucketNodeAssignment>(
        r#"
        SELECT bucket_id, peer_id, allocated_bytes, quota_mode
        FROM bucket_replication_peer
        WHERE bucket_id = ?1 AND quota_mode = 'hard'
        "#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    for row in hard_rows {
        if projected > row.allocated_bytes {
            bail!(
                "quota exceeded on node {} (used {} + {} > allocated {})",
                row.peer_id,
                used,
                net_growth,
                row.allocated_bytes
            );
        }
    }
    Ok(())
}

pub async fn delete_object_by_filename_tx(
    tx: &mut Transaction<'_, Sqlite>,
    filename: &str,
) -> Result<Option<ObjectRecord>> {
    let record = sqlx::query_as::<_, ObjectRecord>(
        r#"SELECT * FROM object WHERE original_filename = ?1 AND deleted_at IS NULL LIMIT 1"#,
    )
    .bind(filename)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(ref r) = record {
        sqlx::query(r#"DELETE FROM object WHERE id = ?1"#)
            .bind(r.id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(record)
}

pub async fn soft_delete_object_by_filename(
    pool: &SqlitePool,
    filename: &str,
    bucket_id: Option<i64>,
) -> Result<Option<ObjectRecord>> {
    let record = match bucket_id {
        Some(bid) => {
            sqlx::query_as::<_, ObjectRecord>(
                r#"
                SELECT * FROM object
                WHERE original_filename = ?1 AND bucket_id = ?2 AND deleted_at IS NULL
                LIMIT 1
                "#,
            )
            .bind(filename)
            .bind(bid)
            .fetch_optional(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, ObjectRecord>(
                r#"
                SELECT * FROM object
                WHERE original_filename = ?1 AND deleted_at IS NULL
                LIMIT 1
                "#,
            )
            .bind(filename)
            .fetch_optional(pool)
            .await?
        }
    };

    if let Some(ref r) = record {
        sqlx::query(
            r#"UPDATE object SET deleted_at = CURRENT_TIMESTAMP, date_modified = CURRENT_TIMESTAMP WHERE id = ?1"#,
        )
        .bind(r.id)
        .execute(pool)
        .await?;
    }
    Ok(record)
}

pub async fn get_object_by_filename(pool: &SqlitePool, filename: &str) -> Result<Option<ObjectRecord>> {
    let record = sqlx::query_as::<_, ObjectRecord>(
        r#"SELECT * FROM object WHERE original_filename = ?1 AND deleted_at IS NULL LIMIT 1"#,
    )
    .bind(filename)
    .fetch_optional(pool)
    .await?;
    Ok(record)
}

pub async fn get_object_by_filename_in_bucket(
    pool: &SqlitePool,
    filename: &str,
    bucket_id: i64,
) -> Result<Option<ObjectRecord>> {
    let record = sqlx::query_as::<_, ObjectRecord>(
        r#"
        SELECT * FROM object
        WHERE original_filename = ?1 AND bucket_id = ?2 AND deleted_at IS NULL
        LIMIT 1
        "#,
    )
    .bind(filename)
    .bind(bucket_id)
    .fetch_optional(pool)
    .await?;
    Ok(record)
}

pub async fn get_object_by_id(pool: &SqlitePool, id: i64) -> Result<Option<ObjectRecord>> {
    let record = sqlx::query_as::<_, ObjectRecord>(r#"SELECT * FROM object WHERE id = ?1"#)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(record)
}

pub async fn list_objects(pool: &SqlitePool) -> Result<Vec<ObjectRecord>> {
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"SELECT * FROM object WHERE deleted_at IS NULL ORDER BY date_uploaded DESC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_objects_in_bucket(pool: &SqlitePool, bucket_id: i64) -> Result<Vec<ObjectRecord>> {
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"
        SELECT * FROM object
        WHERE bucket_id = ?1 AND deleted_at IS NULL
        ORDER BY date_uploaded DESC
        "#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_deleted_objects(
    pool: &SqlitePool,
    bucket_ids: Option<&[i64]>,
) -> Result<Vec<ObjectRecord>> {
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"SELECT * FROM object WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC"#,
    )
    .fetch_all(pool)
    .await?;
    let Some(ids) = bucket_ids else {
        return Ok(rows);
    };
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(rows
        .into_iter()
        .filter(|r| ids.contains(&r.bucket_id))
        .collect())
}

pub async fn restore_object(pool: &SqlitePool, id: i64) -> Result<ObjectRecord> {
    let record = get_object_by_id(pool, id)
        .await?
        .context("object not found")?;
    if record.deleted_at.is_none() {
        bail!("object is not in recycle bin");
    }
    let conflict = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id FROM object
        WHERE bucket_id = ?1 AND original_filename = ?2 AND deleted_at IS NULL AND id != ?3
        LIMIT 1
        "#,
    )
    .bind(record.bucket_id)
    .bind(&record.original_filename)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if conflict.is_some() {
        bail!("an active object with the same key already exists in this bucket");
    }
    sqlx::query(
        r#"UPDATE object SET deleted_at = NULL, date_modified = CURRENT_TIMESTAMP WHERE id = ?1"#,
    )
    .bind(id)
    .execute(pool)
    .await?;
    get_object_by_id(pool, id)
        .await?
        .context("object missing after restore")
}

/// Hard-delete a row (caller should unlink blob + enqueue replication DELETE).
pub async fn hard_delete_object_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: i64,
) -> Result<Option<ObjectRecord>> {
    let record = sqlx::query_as::<_, ObjectRecord>(r#"SELECT * FROM object WHERE id = ?1"#)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(ref r) = record {
        sqlx::query(r#"DELETE FROM object WHERE id = ?1"#)
            .bind(r.id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(record)
}

pub async fn list_expired_deleted_objects(
    pool: &SqlitePool,
    older_than: DateTime<Utc>,
) -> Result<Vec<ObjectRecord>> {
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"
        SELECT * FROM object
        WHERE deleted_at IS NOT NULL AND deleted_at <= ?1
        ORDER BY deleted_at ASC
        "#,
    )
    .bind(older_than)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Escape `%` and `_` for use inside a SQL `LIKE` pattern (with `ESCAPE '\'`).
fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderEntry {
    pub prefix: String,
    pub date_created: Option<DateTime<Utc>>,
    pub date_modified: Option<DateTime<Utc>>,
    pub total_bytes: i64,
    pub object_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrefixListResult {
    pub prefix: String,
    pub delimiter: String,
    pub common_prefixes: Vec<String>,
    pub folders: Vec<FolderEntry>,
    pub objects: Vec<ObjectRecord>,
    /// Total folder+object entries at this listing level (before limit/offset).
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub has_more: bool,
}

/// Aggregate stats for all active objects under `prefix` (prefix should end with `/`).
pub async fn folder_stats(
    pool: &SqlitePool,
    bucket_id: i64,
    prefix: &str,
) -> Result<FolderEntry> {
    let like = format!("{}%", like_escape(prefix));
    let row = sqlx::query(
        r#"
        SELECT
            MIN(date_uploaded) AS date_created,
            MAX(date_modified) AS date_modified,
            COALESCE(SUM(filesize_bytes), 0) AS total_bytes,
            COUNT(*) AS object_count
        FROM object
        WHERE deleted_at IS NULL
          AND bucket_id = ?1
          AND original_filename LIKE ?2 ESCAPE '\'
        "#,
    )
    .bind(bucket_id)
    .bind(&like)
    .fetch_one(pool)
    .await?;

    use sqlx::Row;
    Ok(FolderEntry {
        prefix: prefix.to_string(),
        date_created: row.try_get("date_created")?,
        date_modified: row.try_get("date_modified")?,
        total_bytes: row.try_get("total_bytes")?,
        object_count: row.try_get("object_count")?,
    })
}

pub async fn list_keys_under_prefix(
    pool: &SqlitePool,
    bucket_id: i64,
    prefix: &str,
) -> Result<Vec<ObjectRecord>> {
    let like = format!("{}%", like_escape(prefix));
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"
        SELECT * FROM object
        WHERE deleted_at IS NULL
          AND bucket_id = ?1
          AND original_filename LIKE ?2 ESCAPE '\'
        ORDER BY original_filename ASC
        "#,
    )
    .bind(bucket_id)
    .bind(&like)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Soft-delete every active object under `prefix`. Returns number of rows updated.
pub async fn soft_delete_prefix(
    pool: &SqlitePool,
    bucket_id: i64,
    prefix: &str,
) -> Result<u64> {
    let like = format!("{}%", like_escape(prefix));
    let result = sqlx::query(
        r#"
        UPDATE object
        SET deleted_at = CURRENT_TIMESTAMP, date_modified = CURRENT_TIMESTAMP
        WHERE deleted_at IS NULL
          AND bucket_id = ?1
          AND original_filename LIKE ?2 ESCAPE '\'
        "#,
    )
    .bind(bucket_id)
    .bind(&like)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Rename all keys under `from_prefix` to `to_prefix` (both must end with `/`).
/// Returns Err if any target key already exists.
pub async fn rename_prefix(
    pool: &SqlitePool,
    bucket_id: i64,
    from_prefix: &str,
    to_prefix: &str,
) -> Result<u64> {
    if from_prefix == to_prefix {
        return Ok(0);
    }
    let rows = list_keys_under_prefix(pool, bucket_id, from_prefix).await?;
    if rows.is_empty() {
        bail!("folder is empty or does not exist");
    }

    let mut renames: Vec<(i64, String)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(rest) = row.original_filename.strip_prefix(from_prefix) else {
            continue;
        };
        let new_key = format!("{to_prefix}{rest}");
        let conflict = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT id FROM object
            WHERE bucket_id = ?1
              AND original_filename = ?2
              AND deleted_at IS NULL
            LIMIT 1
            "#,
        )
        .bind(bucket_id)
        .bind(&new_key)
        .fetch_optional(pool)
        .await?;
        if conflict.is_some() {
            bail!("target key already exists: {new_key}");
        }
        renames.push((row.id, new_key));
    }

    let mut tx = pool.begin().await?;
    for (id, new_key) in &renames {
        sqlx::query(
            r#"
            UPDATE object
            SET original_filename = ?1, date_modified = CURRENT_TIMESTAMP
            WHERE id = ?2 AND deleted_at IS NULL
            "#,
        )
        .bind(new_key)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(renames.len() as u64)
}

fn folder_entry_from_rows(prefix: &str, rows: &[ObjectRecord]) -> FolderEntry {
    let mut date_created: Option<DateTime<Utc>> = None;
    let mut date_modified: Option<DateTime<Utc>> = None;
    let mut total_bytes: i64 = 0;
    let mut object_count: i64 = 0;
    for row in rows {
        if !row.original_filename.starts_with(prefix) {
            continue;
        }
        object_count += 1;
        total_bytes += row.filesize_bytes;
        date_created = Some(match date_created {
            Some(d) => d.min(row.date_uploaded),
            None => row.date_uploaded,
        });
        date_modified = Some(match date_modified {
            Some(d) => d.max(row.date_modified),
            None => row.date_modified,
        });
    }
    FolderEntry {
        prefix: prefix.to_string(),
        date_created,
        date_modified,
        total_bytes,
        object_count,
    }
}

fn search_folder_prefixes(q: &str, rows: &[ObjectRecord]) -> Vec<String> {
    let q_lower = q.to_lowercase();
    let mut folders = std::collections::BTreeSet::new();
    for row in rows {
        let key = &row.original_filename;
        let base = key.rsplit('/').next().unwrap_or(key);
        if base == ".keep" {
            if let Some(folder) = key.strip_suffix(".keep") {
                let prefix = if folder.ends_with('/') {
                    folder.to_string()
                } else {
                    format!("{folder}/")
                };
                folders.insert(prefix);
            }
            continue;
        }
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() < 2 {
            continue;
        }
        let mut acc = String::new();
        for (i, seg) in parts.iter().enumerate() {
            if i + 1 == parts.len() {
                break;
            }
            acc.push_str(seg);
            acc.push('/');
            if seg.to_lowercase().contains(&q_lower) {
                folders.insert(acc.clone());
            }
        }
    }
    folders.into_iter().collect()
}

/// List objects with optional substring search or S3-style prefix/delimiter grouping.
/// Soft-deleted objects are always excluded.
///
/// - If `search` is set: return matching objects (bucket-wide) plus matching folders;
///   `.keep` markers are omitted from `objects`.
/// - Else with `delimiter` (typically `/`): emit common prefixes (virtual folders) or
///   objects whose key is exactly under `prefix` (no further delimiter).
/// Slice a folders-then-objects listing window for pagination / infinite scroll.
fn page_folder_object_lists(
    folders: Vec<FolderEntry>,
    objects: Vec<ObjectRecord>,
    prefix: &str,
    delimiter: &str,
    offset: i64,
    limit: i64,
) -> PrefixListResult {
    let offset = offset.max(0) as usize;
    let limit = if limit <= 0 { usize::MAX } else { limit as usize };
    let total = (folders.len() + objects.len()) as i64;
    let end = offset.saturating_add(limit);
    let folder_len = folders.len();

    let page_folders: Vec<FolderEntry> = if offset >= folder_len {
        Vec::new()
    } else {
        folders[offset..end.min(folder_len)].to_vec()
    };
    let page_objects: Vec<ObjectRecord> = if end <= folder_len {
        Vec::new()
    } else {
        let obj_start = offset.saturating_sub(folder_len);
        let obj_end = (end - folder_len).min(objects.len());
        if obj_start >= objects.len() {
            Vec::new()
        } else {
            objects[obj_start..obj_end].to_vec()
        }
    };
    let returned = (page_folders.len() + page_objects.len()) as i64;
    let has_more = (offset as i64) + returned < total;
    let page_prefixes: Vec<String> = page_folders.iter().map(|f| f.prefix.clone()).collect();

    PrefixListResult {
        prefix: prefix.to_string(),
        delimiter: delimiter.to_string(),
        common_prefixes: page_prefixes,
        folders: page_folders,
        objects: page_objects,
        total,
        offset: offset as i64,
        limit: if limit == usize::MAX {
            total.max(0)
        } else {
            limit as i64
        },
        has_more,
    }
}

pub async fn list_objects_with_prefix(
    pool: &SqlitePool,
    prefix: &str,
    delimiter: &str,
    search: Option<&str>,
    bucket_id: Option<i64>,
    offset: i64,
    limit: i64,
) -> Result<PrefixListResult> {
    if let Some(q) = search.filter(|s| !s.is_empty()) {
        let pattern = format!("%{}%", like_escape(q));
        let rows = match bucket_id {
            Some(bid) => {
                sqlx::query_as::<_, ObjectRecord>(
                    r#"
                    SELECT * FROM object
                    WHERE deleted_at IS NULL
                      AND bucket_id = ?2
                      AND original_filename LIKE ?1 ESCAPE '\'
                    ORDER BY original_filename ASC
                    "#,
                )
                .bind(&pattern)
                .bind(bid)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, ObjectRecord>(
                    r#"
                    SELECT * FROM object
                    WHERE deleted_at IS NULL
                      AND original_filename LIKE ?1 ESCAPE '\'
                    ORDER BY original_filename ASC
                    "#,
                )
                .bind(&pattern)
                .fetch_all(pool)
                .await?
            }
        };
        let common_prefixes = search_folder_prefixes(q, &rows);
        let folders: Vec<FolderEntry> = common_prefixes
            .iter()
            .map(|cp| folder_entry_from_rows(cp, &rows))
            .collect();
        let objects: Vec<ObjectRecord> = rows
            .into_iter()
            .filter(|row| {
                let base = row
                    .original_filename
                    .rsplit('/')
                    .next()
                    .unwrap_or(&row.original_filename);
                base != ".keep"
            })
            .collect();
        return Ok(page_folder_object_lists(
            folders, objects, prefix, delimiter, offset, limit,
        ));
    }

    let like = format!("{}%", like_escape(prefix));
    let rows = match bucket_id {
        Some(bid) => {
            sqlx::query_as::<_, ObjectRecord>(
                r#"
                SELECT * FROM object
                WHERE deleted_at IS NULL
                  AND bucket_id = ?2
                  AND original_filename LIKE ?1 ESCAPE '\'
                ORDER BY original_filename ASC
                "#,
            )
            .bind(&like)
            .bind(bid)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, ObjectRecord>(
                r#"
                SELECT * FROM object
                WHERE deleted_at IS NULL
                  AND original_filename LIKE ?1 ESCAPE '\'
                ORDER BY original_filename ASC
                "#,
            )
            .bind(&like)
            .fetch_all(pool)
            .await?
        }
    };

    if delimiter.is_empty() {
        let objects: Vec<ObjectRecord> = rows
            .into_iter()
            .filter(|row| {
                let base = row
                    .original_filename
                    .rsplit('/')
                    .next()
                    .unwrap_or(&row.original_filename);
                base != ".keep"
            })
            .collect();
        return Ok(page_folder_object_lists(
            Vec::new(), objects, prefix, delimiter, offset, limit,
        ));
    }

    let mut common = std::collections::BTreeSet::new();
    let mut objects = Vec::new();

    for row in &rows {
        let key = &row.original_filename;
        if !key.starts_with(prefix) {
            continue;
        }
        let rest = &key[prefix.len()..];
        if let Some(idx) = rest.find(delimiter) {
            common.insert(format!("{}{}", prefix, &rest[..=idx]));
        } else {
            // Object at this level (no further delimiter after prefix).
            let base = key.rsplit('/').next().unwrap_or(key);
            if base != ".keep" {
                objects.push(row.clone());
            }
        }
    }

    let common_prefixes: Vec<String> = common.into_iter().collect();
    let folders: Vec<FolderEntry> = common_prefixes
        .iter()
        .map(|cp| folder_entry_from_rows(cp, &rows))
        .collect();

    Ok(page_folder_object_lists(
        folders, objects, prefix, delimiter, offset, limit,
    ))
}

#[cfg(test)]
mod prefix_list_tests {
    fn classify(prefix: &str, delimiter: &str, keys: &[&str]) -> (Vec<String>, Vec<String>) {
        let mut common = std::collections::BTreeSet::new();
        let mut objects = Vec::new();
        for key in keys {
            if !key.starts_with(prefix) {
                continue;
            }
            let rest = &key[prefix.len()..];
            if let Some(idx) = rest.find(delimiter) {
                common.insert(format!("{}{}", prefix, &rest[..=idx]));
            } else {
                objects.push((*key).to_string());
            }
        }
        (common.into_iter().collect(), objects)
    }

    #[test]
    fn root_virtual_folders_and_files() {
        let (folders, files) = classify(
            "",
            "/",
            &["readme.txt", "photos/cat.jpg", "photos/dog.jpg", "docs/a.pdf"],
        );
        assert_eq!(folders, vec!["docs/", "photos/"]);
        assert_eq!(files, vec!["readme.txt"]);
    }

    #[test]
    fn nested_prefix() {
        let (folders, files) = classify(
            "photos/",
            "/",
            &["photos/cat.jpg", "photos/2024/a.jpg", "photos/2024/b.jpg", "other/x"],
        );
        assert_eq!(folders, vec!["photos/2024/"]);
        assert_eq!(files, vec!["photos/cat.jpg"]);
    }
}

pub async fn insert_object_idempotent(
    pool: &SqlitePool,
    original_filename: &str,
    filepath: &str,
    file_format: &str,
    filesize_bytes: i64,
    etag_type: &str,
    etag: &str,
    date_uploaded: Option<&str>,
    date_modified: Option<&str>,
    bucket_id: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO object (
            original_filename, filepath, file_format, filesize_bytes,
            etag_type, etag, date_uploaded, date_modified, bucket_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                COALESCE(?7, CURRENT_TIMESTAMP),
                COALESCE(?8, CURRENT_TIMESTAMP),
                ?9)
        ON CONFLICT(filepath) DO NOTHING
        "#,
    )
    .bind(original_filename)
    .bind(filepath)
    .bind(file_format)
    .bind(filesize_bytes)
    .bind(etag_type)
    .bind(etag)
    .bind(date_uploaded)
    .bind(date_modified)
    .bind(bucket_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_object_by_filepath(pool: &SqlitePool, filepath: &str) -> Result<bool> {
    let result = sqlx::query(r#"DELETE FROM object WHERE filepath = ?1"#)
        .bind(filepath)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn stats(pool: &SqlitePool) -> Result<(i64, i64, i64)> {
    let bytes: i64 = sqlx::query_scalar(
        r#"SELECT COALESCE(SUM(filesize_bytes), 0) FROM object WHERE deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;
    let count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM object WHERE deleted_at IS NULL"#,
    )
    .fetch_one(pool)
    .await?;
    let pending: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM replication_outbox WHERE status IN ('PENDING', 'FAILED', 'IN_FLIGHT')"#,
    )
    .fetch_one(pool)
    .await?;
    Ok((bytes, count, pending))
}

pub async fn upsert_peer(pool: &SqlitePool, id: &str, endpoint: &str) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO cluster_peer (id, wireguard_endpoint, is_active)
        VALUES (?1, ?2, 1)
        ON CONFLICT(id) DO UPDATE SET
            wireguard_endpoint = excluded.wireguard_endpoint,
            is_active = 1
        "#,
    )
    .bind(id)
    .bind(endpoint)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_active_peers(pool: &SqlitePool) -> Result<Vec<ClusterPeer>> {
    let rows = sqlx::query_as::<_, ClusterPeer>(
        r#"SELECT id, wireguard_endpoint, is_active, last_heartbeat_utc
           FROM cluster_peer WHERE is_active = 1"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn update_peer_heartbeat(pool: &SqlitePool, id: &str, when: DateTime<Utc>) -> Result<()> {
    sqlx::query(
        r#"UPDATE cluster_peer SET last_heartbeat_utc = ?1 WHERE id = ?2"#,
    )
    .bind(when)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn claim_outbox_jobs(pool: &SqlitePool, limit: i64) -> Result<Vec<OutboxJob>> {
    let mut tx = pool.begin().await?;
    let jobs = sqlx::query_as::<_, OutboxJob>(
        r#"
        SELECT
            r.id AS id,
            r.peer_id AS peer_id,
            r.object_id AS object_id,
            r.filepath_uuid AS filepath_uuid,
            r.etag AS etag,
            r.operation AS operation,
            r.status AS status,
            r.attempt_count AS attempt_count,
            p.wireguard_endpoint AS wireguard_endpoint,
            o.original_filename AS original_filename,
            COALESCE(o.filepath, r.filepath_uuid) AS filepath,
            o.file_format AS file_format,
            o.filesize_bytes AS filesize_bytes,
            o.etag_type AS etag_type,
            COALESCE(o.etag, r.etag) AS object_etag,
            o.date_uploaded AS date_uploaded,
            o.date_modified AS date_modified,
            b.name AS bucket_name
        FROM replication_outbox r
        JOIN cluster_peer p ON r.peer_id = p.id
        LEFT JOIN object o ON r.object_id = o.id
        LEFT JOIN bucket b ON b.id = o.bucket_id
        WHERE r.status IN ('PENDING', 'FAILED')
          AND r.next_retry_utc <= CURRENT_TIMESTAMP
          AND p.is_active = 1
        ORDER BY r.id ASC
        LIMIT ?1
        "#,
    )
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;

    for job in &jobs {
        sqlx::query(r#"UPDATE replication_outbox SET status = 'IN_FLIGHT' WHERE id = ?1"#)
            .bind(job.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(jobs)
}

pub async fn complete_outbox_job(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(r#"DELETE FROM replication_outbox WHERE id = ?1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn fail_outbox_job(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE replication_outbox
        SET status = 'FAILED',
            attempt_count = attempt_count + 1,
            next_retry_utc = datetime('now', '+' || ((attempt_count + 1) * 30) || ' seconds')
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn digest_map(pool: &SqlitePool, prefix: &str) -> Result<HashMap<String, String>> {
    let rows = if prefix.is_empty() {
        sqlx::query_as::<_, (String, String)>(
            r#"SELECT filepath, etag FROM object WHERE deleted_at IS NULL"#,
        )
        .fetch_all(pool)
        .await?
    } else {
        let like = format!("{prefix}%");
        sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT filepath, etag FROM object
            WHERE deleted_at IS NULL
              AND (filepath LIKE ?1 OR original_filename LIKE ?1)
            "#,
        )
        .bind(like)
        .fetch_all(pool)
        .await?
    };
    Ok(rows.into_iter().collect())
}

pub async fn enqueue_put_for_peer(
    pool: &SqlitePool,
    peer_id: &str,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
) -> Result<()> {
    // Avoid duplicate pending jobs for same peer+object+PUT
    let exists: Option<i64> = sqlx::query_scalar(
        r#"
        SELECT id FROM replication_outbox
        WHERE peer_id = ?1 AND filepath_uuid = ?2 AND operation = 'PUT'
          AND status IN ('PENDING', 'FAILED', 'IN_FLIGHT')
        LIMIT 1
        "#,
    )
    .bind(peer_id)
    .bind(filepath_uuid)
    .fetch_optional(pool)
    .await?;

    if exists.is_some() {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO replication_outbox (peer_id, object_id, filepath_uuid, etag, operation)
        VALUES (?1, ?2, ?3, ?4, 'PUT')
        "#,
    )
    .bind(peer_id)
    .bind(object_id)
    .bind(filepath_uuid)
    .bind(etag)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn object_by_filepath(pool: &SqlitePool, filepath: &str) -> Result<Option<ObjectRecord>> {
    let row = sqlx::query_as::<_, ObjectRecord>(
        r#"SELECT * FROM object WHERE filepath = ?1 AND deleted_at IS NULL LIMIT 1"#,
    )
    .bind(filepath)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Resolve default `storage` bucket id (created by migration seed).
pub async fn default_bucket_id(pool: &SqlitePool) -> Result<i64> {
    let id: Option<i64> = sqlx::query_scalar(
        r#"SELECT id FROM bucket WHERE name = 'storage' LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await?;
    id.context("default storage bucket missing")
}

pub async fn bucket_etag_type(pool: &SqlitePool, bucket_id: i64) -> Result<EtagType> {
    let s: Option<String> =
        sqlx::query_scalar(r#"SELECT etag_type FROM bucket WHERE id = ?1"#)
            .bind(bucket_id)
            .fetch_optional(pool)
            .await?;
    let Some(s) = s else {
        bail!("bucket not found");
    };
    EtagType::from_str(&s).map_err(|_| anyhow::anyhow!("invalid bucket etag_type {s}"))
}

pub async fn set_bucket_etag_type(
    pool: &SqlitePool,
    bucket_id: i64,
    etag_type: EtagType,
) -> Result<()> {
    let result = sqlx::query(r#"UPDATE bucket SET etag_type = ?1 WHERE id = ?2"#)
        .bind(etag_type.to_string())
        .bind(bucket_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("bucket not found");
    }
    Ok(())
}

pub async fn begin_bucket_etag_rehash(
    pool: &SqlitePool,
    bucket_id: i64,
    total: i64,
) -> Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE bucket
        SET etag_rehash_status = 'running',
            etag_rehash_processed = 0,
            etag_rehash_total = ?1,
            etag_rehash_error = NULL
        WHERE id = ?2
        "#,
    )
    .bind(total)
    .bind(bucket_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("bucket not found");
    }
    Ok(())
}

pub async fn bump_bucket_etag_rehash_processed(
    pool: &SqlitePool,
    bucket_id: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE bucket
        SET etag_rehash_processed = etag_rehash_processed + 1
        WHERE id = ?1
        "#,
    )
    .bind(bucket_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn finish_bucket_etag_rehash(
    pool: &SqlitePool,
    bucket_id: i64,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE bucket
        SET etag_rehash_status = ?1,
            etag_rehash_error = ?2
        WHERE id = ?3
        "#,
    )
    .bind(status)
    .bind(error)
    .bind(bucket_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_object_etag(
    pool: &SqlitePool,
    object_id: i64,
    etag_type: EtagType,
    etag: &str,
) -> Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE object
        SET etag_type = ?1,
            etag = ?2,
            date_modified = CURRENT_TIMESTAMP
        WHERE id = ?3 AND deleted_at IS NULL
        "#,
    )
    .bind(etag_type.to_string())
    .bind(etag)
    .bind(object_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("object not found");
    }
    Ok(())
}

pub async fn count_active_objects_in_bucket(pool: &SqlitePool, bucket_id: i64) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM object
        WHERE bucket_id = ?1
          AND deleted_at IS NULL
          AND original_filename != '.keep'
          AND original_filename NOT LIKE '%/.keep'
        "#,
    )
    .bind(bucket_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Active objects in a bucket, excluding folder `.keep` markers (folders have no ETags).
pub async fn list_objects_for_etag_rehash(
    pool: &SqlitePool,
    bucket_id: i64,
) -> Result<Vec<ObjectRecord>> {
    let rows = sqlx::query_as::<_, ObjectRecord>(
        r#"
        SELECT * FROM object
        WHERE bucket_id = ?1
          AND deleted_at IS NULL
          AND original_filename != '.keep'
          AND original_filename NOT LIKE '%/.keep'
        ORDER BY date_uploaded DESC
        "#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
