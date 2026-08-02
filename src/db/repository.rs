use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::models::{ClusterPeer, EtagType, ObjectRecord, OutboxJob};

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
pub async fn enqueue_put_for_active_peers(
    tx: &mut Transaction<'_, Sqlite>,
    object_id: i64,
    filepath_uuid: &str,
    etag: &str,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO replication_outbox (peer_id, object_id, filepath_uuid, etag, operation)
        SELECT p.id, ?1, ?2, ?3, 'PUT'
        FROM cluster_peer p
        JOIN object o ON o.id = ?1
        JOIN bucket b ON b.id = o.bucket_id
        WHERE p.is_active = 1
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
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO replication_outbox (peer_id, object_id, filepath_uuid, etag, operation)
        SELECT p.id, ?1, ?2, ?3, 'DELETE'
        FROM cluster_peer p
        JOIN object o ON o.id = ?1
        JOIN bucket b ON b.id = o.bucket_id
        WHERE p.is_active = 1
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
                INSERT OR IGNORE INTO bucket_replication_peer (bucket_id, peer_id)
                VALUES (?1, ?2)
                "#,
            )
            .bind(bucket_id)
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
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
    /// ETag of the `{prefix}.keep` marker when present.
    pub etag: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrefixListResult {
    pub prefix: String,
    pub delimiter: String,
    pub common_prefixes: Vec<String>,
    pub folders: Vec<FolderEntry>,
    pub objects: Vec<ObjectRecord>,
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
    let keep_key = format!("{prefix}.keep");
    let etag = sqlx::query_scalar::<_, String>(
        r#"
        SELECT etag FROM object
        WHERE deleted_at IS NULL
          AND bucket_id = ?1
          AND original_filename = ?2
        LIMIT 1
        "#,
    )
    .bind(bucket_id)
    .bind(&keep_key)
    .fetch_optional(pool)
    .await?;

    Ok(FolderEntry {
        prefix: prefix.to_string(),
        date_created: row.try_get("date_created")?,
        date_modified: row.try_get("date_modified")?,
        total_bytes: row.try_get("total_bytes")?,
        object_count: row.try_get("object_count")?,
        etag,
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
    let keep_key = format!("{prefix}.keep");
    let mut etag: Option<String> = None;
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
        if row.original_filename == keep_key {
            etag = Some(row.etag.clone());
        }
    }
    FolderEntry {
        prefix: prefix.to_string(),
        date_created,
        date_modified,
        total_bytes,
        object_count,
        etag,
    }
}

/// List objects with optional substring search or S3-style prefix/delimiter grouping.
/// Soft-deleted objects are always excluded.
///
/// - If `search` is set: return matching objects as a flat list; `common_prefixes` empty.
/// - Else with `delimiter` (typically `/`): emit common prefixes (virtual folders) or
///   objects whose key is exactly under `prefix` (no further delimiter).
pub async fn list_objects_with_prefix(
    pool: &SqlitePool,
    prefix: &str,
    delimiter: &str,
    search: Option<&str>,
    bucket_id: Option<i64>,
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
        return Ok(PrefixListResult {
            prefix: prefix.to_string(),
            delimiter: delimiter.to_string(),
            common_prefixes: Vec::new(),
            folders: Vec::new(),
            objects: rows,
        });
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
        return Ok(PrefixListResult {
            prefix: prefix.to_string(),
            delimiter: delimiter.to_string(),
            common_prefixes: Vec::new(),
            folders: Vec::new(),
            objects: rows,
        });
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
            objects.push(row.clone());
        }
    }

    let common_prefixes: Vec<String> = common.into_iter().collect();
    let folders: Vec<FolderEntry> = common_prefixes
        .iter()
        .map(|cp| folder_entry_from_rows(cp, &rows))
        .collect();

    Ok(PrefixListResult {
        prefix: prefix.to_string(),
        delimiter: delimiter.to_string(),
        common_prefixes,
        folders,
        objects,
    })
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
