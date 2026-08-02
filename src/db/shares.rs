use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use rand::Rng;
use sqlx::SqlitePool;

use super::models::{ShareAccessMode, ShareLinkRecord, ShareTargetKind};
use super::repository;

/// Base58-like charset (no 0, O, I, l) for tokens and short codes.
pub const SHARE_CHARSET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub const TOKEN_LEN: usize = 22;
pub const SHORT_CODE_LEN: usize = 8;

pub fn random_share_code(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..SHARE_CHARSET.len());
            SHARE_CHARSET[idx] as char
        })
        .collect()
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub async fn prefix_has_objects(
    pool: &SqlitePool,
    bucket_id: i64,
    prefix: &str,
) -> Result<bool> {
    let like = format!("{}%", like_escape(prefix));
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM object
        WHERE deleted_at IS NULL
          AND bucket_id = ?1
          AND original_filename LIKE ?2 ESCAPE '\'
        "#,
    )
    .bind(bucket_id)
    .bind(&like)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

pub async fn create_share(
    pool: &SqlitePool,
    bucket_id: i64,
    target_key: &str,
    target_kind: ShareTargetKind,
    access_mode: ShareAccessMode,
    expires_at: Option<DateTime<Utc>>,
    created_by_account_id: i64,
    account_ids: &[i64],
    shorten: bool,
) -> Result<ShareLinkRecord> {
    match target_kind {
        ShareTargetKind::File => {
            if repository::get_object_by_filename_in_bucket(pool, target_key, bucket_id)
                .await?
                .is_none()
            {
                bail!("object not found");
            }
        }
        ShareTargetKind::Folder => {
            if !prefix_has_objects(pool, bucket_id, target_key).await? {
                bail!("folder not found or empty");
            }
        }
    }

    if access_mode == ShareAccessMode::SpecificUsers && account_ids.is_empty() {
        bail!("specific_users requires at least one account_id");
    }

    let mut token = random_share_code(TOKEN_LEN);
    let mut short_code = if shorten {
        Some(random_share_code(SHORT_CODE_LEN))
    } else {
        None
    };

    // Retry on rare unique collisions.
    for _ in 0..8 {
        let insert = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO share_link (
                token, short_code, bucket_id, target_key, target_kind,
                access_mode, expires_at, created_by_account_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            RETURNING id
            "#,
        )
        .bind(&token)
        .bind(&short_code)
        .bind(bucket_id)
        .bind(target_key)
        .bind(target_kind)
        .bind(access_mode)
        .bind(expires_at)
        .bind(created_by_account_id)
        .fetch_one(pool)
        .await;

        match insert {
            Ok(id) => {
                if access_mode == ShareAccessMode::SpecificUsers {
                    for account_id in account_ids {
                        sqlx::query(
                            r#"
                            INSERT INTO share_link_user (share_id, account_id)
                            VALUES (?1, ?2)
                            "#,
                        )
                        .bind(id)
                        .bind(account_id)
                        .execute(pool)
                        .await?;
                    }
                }
                return get_share_by_id(pool, id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("share missing after insert"));
            }
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("UNIQUE") {
                    token = random_share_code(TOKEN_LEN);
                    if shorten {
                        short_code = Some(random_share_code(SHORT_CODE_LEN));
                    }
                    continue;
                }
                return Err(err.into());
            }
        }
    }
    bail!("failed to generate unique share token");
}

pub async fn get_share_by_id(pool: &SqlitePool, id: i64) -> Result<Option<ShareLinkRecord>> {
    let row = sqlx::query_as::<_, ShareLinkRecord>(
        r#"SELECT * FROM share_link WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn get_share_by_token(pool: &SqlitePool, token: &str) -> Result<Option<ShareLinkRecord>> {
    let row = sqlx::query_as::<_, ShareLinkRecord>(
        r#"SELECT * FROM share_link WHERE token = ?1"#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn get_share_by_short_code(
    pool: &SqlitePool,
    code: &str,
) -> Result<Option<ShareLinkRecord>> {
    let row = sqlx::query_as::<_, ShareLinkRecord>(
        r#"SELECT * FROM share_link WHERE short_code = ?1"#,
    )
    .bind(code)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Active share: not revoked and not past expires_at.
pub fn share_is_usable(share: &ShareLinkRecord, now: DateTime<Utc>) -> Result<(), ShareDenyReason> {
    if share.revoked_at.is_some() {
        return Err(ShareDenyReason::Revoked);
    }
    if let Some(exp) = share.expires_at {
        if exp < now {
            return Err(ShareDenyReason::Expired);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareDenyReason {
    Revoked,
    Expired,
}

pub async fn list_share_recipients(pool: &SqlitePool, share_id: i64) -> Result<Vec<i64>> {
    let rows = sqlx::query_scalar::<_, i64>(
        r#"SELECT account_id FROM share_link_user WHERE share_id = ?1 ORDER BY account_id"#,
    )
    .bind(share_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn share_allows_account(pool: &SqlitePool, share_id: i64, account_id: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM share_link_user
        WHERE share_id = ?1 AND account_id = ?2
        "#,
    )
    .bind(share_id)
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

pub async fn list_shares_for_target(
    pool: &SqlitePool,
    bucket_id: i64,
    target_key: &str,
    created_by_account_id: i64,
) -> Result<Vec<ShareLinkRecord>> {
    let rows = sqlx::query_as::<_, ShareLinkRecord>(
        r#"
        SELECT * FROM share_link
        WHERE bucket_id = ?1
          AND target_key = ?2
          AND created_by_account_id = ?3
          AND revoked_at IS NULL
        ORDER BY created_at DESC
        "#,
    )
    .bind(bucket_id)
    .bind(target_key)
    .bind(created_by_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Active (non-revoked) shares that cover `key` in `bucket_id`: exact file shares or parent folders.
/// Caller should filter with `share_is_usable`. Prefer public, then exact file match.
pub async fn list_shares_covering_key(
    pool: &SqlitePool,
    bucket_id: i64,
    key: &str,
) -> Result<Vec<ShareLinkRecord>> {
    let rows = sqlx::query_as::<_, ShareLinkRecord>(
        r#"
        SELECT * FROM share_link
        WHERE bucket_id = ?1
          AND revoked_at IS NULL
          AND (
            (target_kind = 'file' AND target_key = ?2)
            OR (target_kind = 'folder' AND ?2 LIKE (target_key || '%'))
          )
        ORDER BY created_at DESC
        "#,
    )
    .bind(bucket_id)
    .bind(key)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Active folder shares with exact `target_key` prefix.
pub async fn list_folder_shares_for_prefix(
    pool: &SqlitePool,
    bucket_id: i64,
    prefix: &str,
) -> Result<Vec<ShareLinkRecord>> {
    let rows = sqlx::query_as::<_, ShareLinkRecord>(
        r#"
        SELECT * FROM share_link
        WHERE bucket_id = ?1
          AND revoked_at IS NULL
          AND target_kind = 'folder'
          AND target_key = ?2
        ORDER BY created_at DESC
        "#,
    )
    .bind(bucket_id)
    .bind(prefix)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Pick best covering share: public first, then exact file match, then newest.
pub fn prefer_share_for_link<'a>(
    shares: &'a [ShareLinkRecord],
    file_key: Option<&str>,
) -> Option<&'a ShareLinkRecord> {
    let now = Utc::now();
    let usable: Vec<&ShareLinkRecord> = shares
        .iter()
        .filter(|s| share_is_usable(s, now).is_ok())
        .collect();
    if usable.is_empty() {
        return None;
    }
    if let Some(s) = usable
        .iter()
        .find(|s| s.access_mode == ShareAccessMode::Public)
    {
        return Some(*s);
    }
    if let Some(key) = file_key {
        if let Some(s) = usable
            .iter()
            .find(|s| s.target_kind == ShareTargetKind::File && s.target_key == key)
        {
            return Some(*s);
        }
    }
    usable.into_iter().next()
}

pub async fn revoke_share(
    pool: &SqlitePool,
    share_id: i64,
    actor_account_id: i64,
    is_owner: bool,
) -> Result<bool> {
    let share = match get_share_by_id(pool, share_id).await? {
        Some(s) => s,
        None => return Ok(false),
    };
    if share.revoked_at.is_some() {
        return Ok(true);
    }
    if !is_owner && share.created_by_account_id != actor_account_id {
        bail!("not allowed to revoke this share");
    }
    sqlx::query(
        r#"
        UPDATE share_link
        SET revoked_at = CURRENT_TIMESTAMP
        WHERE id = ?1 AND revoked_at IS NULL
        "#,
    )
    .bind(share_id)
    .execute(pool)
    .await?;
    Ok(true)
}

/// True if `key` is allowed by this share (exact file or under folder prefix).
pub fn key_in_share_scope(share: &ShareLinkRecord, key: &str) -> bool {
    match share.target_kind {
        ShareTargetKind::File => key == share.target_key,
        ShareTargetKind::Folder => key.starts_with(&share.target_key),
    }
}

/// Resolve a listing prefix for a folder share. `relative` is optional path under the share root.
pub fn resolve_list_prefix(
    share: &ShareLinkRecord,
    relative: Option<&str>,
) -> Result<String, &'static str> {
    if share.target_kind != ShareTargetKind::Folder {
        return Err("share is not a folder");
    }
    let root = &share.target_key;
    let Some(rel) = relative.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(root.clone());
    };
    let rel = rel.trim_start_matches('/');
    let joined = if rel.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}{rel}/")
    };
    // Confinement: must stay under root.
    if !joined.starts_with(root.as_str()) {
        return Err("prefix outside share scope");
    }
    // Reject path traversal segments.
    for segment in joined.trim_end_matches('/').split('/') {
        if segment == "." || segment == ".." || segment.is_empty() {
            return Err("invalid prefix");
        }
    }
    Ok(joined)
}

pub fn share_url_path(share: &ShareLinkRecord) -> String {
    if let Some(code) = share.short_code.as_deref() {
        format!("/s/{code}")
    } else {
        format!("/share/{}", share.token)
    }
}
