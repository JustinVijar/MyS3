use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::fs;

use crate::storage::StorageEngine;

const MISSING_CAP: usize = 50;
const ORPHAN_CAP: usize = 50;

#[derive(Debug, Clone, Serialize)]
pub struct IntegrityReport {
    pub db_rows: i64,
    pub db_active: i64,
    pub db_recycled: i64,
    pub disk_files_before: i64,
    pub disk_files_after: i64,
    pub active_ok: i64,
    pub recycle_ok: i64,
    pub orphans_removed: Vec<String>,
    pub orphans_removed_count: i64,
    pub missing_active: Vec<String>,
    pub missing_active_count: i64,
    pub missing_recycle: Vec<String>,
    pub missing_recycle_count: i64,
    pub repaired: bool,
}

#[derive(Debug, Clone)]
struct DbObjectFile {
    filepath: String,
    deleted: bool,
}

/// Normalize a DB or disk path to a relative key under storage root: `objects/<name>`.
pub fn normalize_object_rel(filepath: &str) -> String {
    let trimmed = filepath.trim().trim_start_matches("./");
    if let Some(rest) = trimmed.strip_prefix("objects/") {
        format!("objects/{rest}")
    } else if trimmed.contains('/') {
        trimmed.to_string()
    } else {
        format!("objects/{trimmed}")
    }
}

async fn list_db_object_files(pool: &SqlitePool) -> Result<Vec<DbObjectFile>> {
    use sqlx::Row;
    let rows = sqlx::query(r#"SELECT filepath, deleted_at FROM object"#)
        .fetch_all(pool)
        .await
        .context("list object filepaths")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let filepath: String = row.try_get("filepath")?;
        let deleted_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("deleted_at")?;
        out.push(DbObjectFile {
            filepath: normalize_object_rel(&filepath),
            deleted: deleted_at.is_some(),
        });
    }
    Ok(out)
}

async fn list_disk_object_files(objects_dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if !objects_dir.exists() {
        return Ok(out);
    }
    let mut rd = fs::read_dir(objects_dir)
        .await
        .with_context(|| format!("read_dir {}", objects_dir.display()))?;
    while let Some(entry) = rd.next_entry().await? {
        let meta = entry.metadata().await?;
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        out.push(format!("objects/{name}"));
    }
    out.sort();
    Ok(out)
}

async fn classify(
    pool: &SqlitePool,
    engine: &StorageEngine,
) -> Result<(
    Vec<DbObjectFile>,
    HashSet<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    i64,
    i64,
)> {
    let db_files = list_db_object_files(pool).await?;
    let disk = list_disk_object_files(engine.objects_dir()).await?;
    let disk_set: HashSet<String> = disk.iter().cloned().collect();

    let mut db_by_path: HashMap<String, bool> = HashMap::new();
    for row in &db_files {
        // If both active and recycled exist for same path (shouldn't), prefer active.
        db_by_path
            .entry(row.filepath.clone())
            .and_modify(|deleted| *deleted = *deleted && row.deleted)
            .or_insert(row.deleted);
    }

    let mut orphans = Vec::new();
    for path in &disk {
        if !db_by_path.contains_key(path) {
            orphans.push(path.clone());
        }
    }
    orphans.sort();

    let mut missing_active = Vec::new();
    let mut missing_recycle = Vec::new();
    let mut active_ok = 0i64;
    let mut recycle_ok = 0i64;
    for row in &db_files {
        if disk_set.contains(&row.filepath) {
            if row.deleted {
                recycle_ok += 1;
            } else {
                active_ok += 1;
            }
        } else if row.deleted {
            missing_recycle.push(row.filepath.clone());
        } else {
            missing_active.push(row.filepath.clone());
        }
    }
    missing_active.sort();
    missing_recycle.sort();

    Ok((
        db_files,
        disk_set,
        orphans,
        missing_active,
        missing_recycle,
        active_ok,
        recycle_ok,
    ))
}

fn capped(mut items: Vec<String>, cap: usize) -> (Vec<String>, i64) {
    let total = items.len() as i64;
    if items.len() > cap {
        items.truncate(cap);
    }
    (items, total)
}

/// Dry-run integrity check (no deletes).
pub async fn inspect(pool: &SqlitePool, engine: &StorageEngine) -> Result<IntegrityReport> {
    let (db_files, disk_set, orphans, missing_active, missing_recycle, active_ok, recycle_ok) =
        classify(pool, engine).await?;
    let db_active = db_files.iter().filter(|r| !r.deleted).count() as i64;
    let db_recycled = db_files.len() as i64 - db_active;
    let (orphans_removed, orphans_removed_count) = capped(orphans, ORPHAN_CAP);
    let (missing_active, missing_active_count) = capped(missing_active, MISSING_CAP);
    let (missing_recycle, missing_recycle_count) = capped(missing_recycle, MISSING_CAP);
    let disk_files = disk_set.len() as i64;

    Ok(IntegrityReport {
        db_rows: db_files.len() as i64,
        db_active,
        db_recycled,
        disk_files_before: disk_files,
        disk_files_after: disk_files,
        active_ok,
        recycle_ok,
        orphans_removed,
        orphans_removed_count,
        missing_active,
        missing_active_count,
        missing_recycle,
        missing_recycle_count,
        repaired: false,
    })
}

/// Remove orphan files; report missing blobs.
pub async fn reconcile(pool: &SqlitePool, engine: &StorageEngine) -> Result<IntegrityReport> {
    let (db_files, disk_set, orphans, missing_active, missing_recycle, active_ok, recycle_ok) =
        classify(pool, engine).await?;
    let db_active = db_files.iter().filter(|r| !r.deleted).count() as i64;
    let db_recycled = db_files.len() as i64 - db_active;
    let disk_before = disk_set.len() as i64;

    let mut removed = Vec::new();
    for rel in &orphans {
        if let Err(err) = engine.unlink(rel).await {
            tracing::warn!("failed to unlink orphan {rel}: {err:#}");
            continue;
        }
        removed.push(rel.clone());
    }

    let disk_after = list_disk_object_files(engine.objects_dir()).await?.len() as i64;
    let orphans_removed_count = removed.len() as i64;
    let (orphans_removed, _) = capped(removed, ORPHAN_CAP);
    let (missing_active, missing_active_count) = capped(missing_active, MISSING_CAP);
    let (missing_recycle, missing_recycle_count) = capped(missing_recycle, MISSING_CAP);
    let _ = disk_set;

    Ok(IntegrityReport {
        db_rows: db_files.len() as i64,
        db_active,
        db_recycled,
        disk_files_before: disk_before,
        disk_files_after: disk_after,
        active_ok,
        recycle_ok,
        orphans_removed,
        orphans_removed_count,
        missing_active,
        missing_active_count,
        missing_recycle,
        missing_recycle_count,
        repaired: true,
    })
}

/// Count files currently on disk under objects/.
pub async fn count_disk_files(objects_dir: &Path) -> Result<i64> {
    Ok(list_disk_object_files(objects_dir).await?.len() as i64)
}

#[allow(dead_code)]
pub fn absolute_for_rel(storage_root: &Path, rel: &str) -> PathBuf {
    storage_root.join(rel)
}
