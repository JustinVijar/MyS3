use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sqlx::SqlitePool;

use super::models::{
    AccountRecord, AppSettings, BucketRecord, CrudAction, CrudPerms, RetentionUnit, RoleBucketPermission,
    RoleRecord,
};

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash password: {e}"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, password_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(password_hash)
        .map_err(|e| anyhow::anyhow!("parse password hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn generate_credentials() -> (String, String) {
    // 16 bytes → 32 hex chars username; 32 bytes → 64 hex chars password
    (random_hex(16), random_hex(32))
}

pub fn generate_session_token() -> String {
    random_hex(32)
}

pub async fn account_count(pool: &SqlitePool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM account"#)
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn get_account_by_id(pool: &SqlitePool, id: i64) -> Result<Option<AccountRecord>> {
    let row = sqlx::query_as::<_, AccountRecord>(r#"SELECT * FROM account WHERE id = ?1"#)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn get_account_by_username(
    pool: &SqlitePool,
    username_hex: &str,
) -> Result<Option<AccountRecord>> {
    let row = sqlx::query_as::<_, AccountRecord>(
        r#"SELECT * FROM account WHERE username_hex = ?1 LIMIT 1"#,
    )
    .bind(username_hex)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn list_accounts(pool: &SqlitePool) -> Result<Vec<AccountRecord>> {
    let rows = sqlx::query_as::<_, AccountRecord>(
        r#"SELECT * FROM account ORDER BY created_utc ASC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn create_account(
    pool: &SqlitePool,
    username_hex: &str,
    password_plain: &str,
    display_name: &str,
    created_by_account_id: Option<i64>,
) -> Result<AccountRecord> {
    let password_hash = hash_password(password_plain)?;
    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO account (username_hex, password_hash, display_name, created_by_account_id)
        VALUES (?1, ?2, ?3, ?4)
        RETURNING id
        "#,
    )
    .bind(username_hex)
    .bind(&password_hash)
    .bind(display_name)
    .bind(created_by_account_id)
    .fetch_one(pool)
    .await?;
    get_account_by_id(pool, id)
        .await?
        .context("account missing after insert")
}

/// True if `actor_id` created `account_id` (and may delete it).
pub async fn account_created_by(
    pool: &SqlitePool,
    account_id: i64,
    actor_id: i64,
) -> Result<bool> {
    let account = get_account_by_id(pool, account_id).await?;
    Ok(account.and_then(|a| a.created_by_account_id) == Some(actor_id))
}

pub async fn set_account_disabled(pool: &SqlitePool, id: i64, disabled: bool) -> Result<()> {
    let result = sqlx::query(
        r#"UPDATE account SET is_disabled = ?1, updated_utc = CURRENT_TIMESTAMP WHERE id = ?2"#,
    )
    .bind(disabled)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("account not found");
    }
    Ok(())
}

pub async fn update_account_display_name(
    pool: &SqlitePool,
    id: i64,
    display_name: &str,
) -> Result<()> {
    let result = sqlx::query(
        r#"UPDATE account SET display_name = ?1, updated_utc = CURRENT_TIMESTAMP WHERE id = ?2"#,
    )
    .bind(display_name)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("account not found");
    }
    Ok(())
}

pub async fn set_account_password(pool: &SqlitePool, id: i64, password_plain: &str) -> Result<()> {
    let password_hash = hash_password(password_plain)?;
    let result = sqlx::query(
        r#"UPDATE account SET password_hash = ?1, updated_utc = CURRENT_TIMESTAMP WHERE id = ?2"#,
    )
    .bind(password_hash)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("account not found");
    }
    Ok(())
}

pub async fn delete_account(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query(r#"DELETE FROM account WHERE id = ?1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_roles(pool: &SqlitePool) -> Result<Vec<RoleRecord>> {
    let rows = sqlx::query_as::<_, RoleRecord>(
        r#"SELECT * FROM role ORDER BY position DESC, id ASC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_role(pool: &SqlitePool, id: i64) -> Result<Option<RoleRecord>> {
    let row = sqlx::query_as::<_, RoleRecord>(r#"SELECT * FROM role WHERE id = ?1"#)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn get_owner_role(pool: &SqlitePool) -> Result<Option<RoleRecord>> {
    let row = sqlx::query_as::<_, RoleRecord>(
        r#"SELECT * FROM role WHERE is_owner = 1 ORDER BY id ASC LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn create_role(pool: &SqlitePool, name: &str, position: i64) -> Result<RoleRecord> {
    let id = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO role (name, position, is_owner) VALUES (?1, ?2, 0) RETURNING id"#,
    )
    .bind(name)
    .bind(position)
    .fetch_one(pool)
    .await?;
    get_role(pool, id).await?.context("role missing after insert")
}

pub async fn update_role(
    pool: &SqlitePool,
    id: i64,
    name: Option<&str>,
    position: Option<i64>,
) -> Result<()> {
    let role = get_role(pool, id).await?.context("role not found")?;
    let name = name.unwrap_or(&role.name);
    let position = position.unwrap_or(role.position);
    sqlx::query(r#"UPDATE role SET name = ?1, position = ?2 WHERE id = ?3"#)
        .bind(name)
        .bind(position)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_role(pool: &SqlitePool, id: i64) -> Result<()> {
    let role = get_role(pool, id).await?.context("role not found")?;
    if role.is_owner {
        bail!("cannot delete Owner role");
    }
    sqlx::query(r#"DELETE FROM role WHERE id = ?1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_account_role_ids(pool: &SqlitePool, account_id: i64) -> Result<Vec<i64>> {
    let rows = sqlx::query_scalar::<_, i64>(
        r#"SELECT role_id FROM account_role WHERE account_id = ?1"#,
    )
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn set_account_roles(pool: &SqlitePool, account_id: i64, role_ids: &[i64]) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(r#"DELETE FROM account_role WHERE account_id = ?1"#)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    for rid in role_ids {
        sqlx::query(r#"INSERT INTO account_role (account_id, role_id) VALUES (?1, ?2)"#)
            .bind(account_id)
            .bind(rid)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn assign_role(pool: &SqlitePool, account_id: i64, role_id: i64) -> Result<()> {
    sqlx::query(
        r#"INSERT OR IGNORE INTO account_role (account_id, role_id) VALUES (?1, ?2)"#,
    )
    .bind(account_id)
    .bind(role_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_role_permissions(
    pool: &SqlitePool,
    role_id: i64,
) -> Result<Vec<RoleBucketPermission>> {
    let rows = sqlx::query_as::<_, RoleBucketPermission>(
        r#"SELECT * FROM role_bucket_permission WHERE role_id = ?1"#,
    )
    .bind(role_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_all_permissions(pool: &SqlitePool) -> Result<Vec<RoleBucketPermission>> {
    let rows = sqlx::query_as::<_, RoleBucketPermission>(
        r#"SELECT * FROM role_bucket_permission ORDER BY role_id, bucket_id"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn upsert_role_permission(
    pool: &SqlitePool,
    role_id: i64,
    bucket_id: i64,
    perms: CrudPerms,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO role_bucket_permission (
            role_id, bucket_id, can_create, can_read, can_update, can_delete
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(role_id, bucket_id) DO UPDATE SET
            can_create = excluded.can_create,
            can_read = excluded.can_read,
            can_update = excluded.can_update,
            can_delete = excluded.can_delete
        "#,
    )
    .bind(role_id)
    .bind(bucket_id)
    .bind(perms.can_create)
    .bind(perms.can_read)
    .bind(perms.can_update)
    .bind(perms.can_delete)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn replace_role_permissions(
    pool: &SqlitePool,
    role_id: i64,
    perms: &[(i64, CrudPerms)],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(r#"DELETE FROM role_bucket_permission WHERE role_id = ?1"#)
        .bind(role_id)
        .execute(&mut *tx)
        .await?;
    for (bucket_id, p) in perms {
        sqlx::query(
            r#"
            INSERT INTO role_bucket_permission (
                role_id, bucket_id, can_create, can_read, can_update, can_delete
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
        )
        .bind(role_id)
        .bind(bucket_id)
        .bind(p.can_create)
        .bind(p.can_read)
        .bind(p.can_update)
        .bind(p.can_delete)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn account_is_owner(pool: &SqlitePool, account_id: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM account_role ar
        JOIN role r ON r.id = ar.role_id
        WHERE ar.account_id = ?1 AND r.is_owner = 1
        "#,
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

pub async fn account_owns_bucket(
    pool: &SqlitePool,
    account_id: i64,
    bucket_id: i64,
) -> Result<bool> {
    let bucket = get_bucket_by_id(pool, bucket_id).await?;
    Ok(bucket.and_then(|b| b.owner_account_id) == Some(account_id))
}

/// Bucket owner or anyone with Update (can-edit) may configure replication.
pub async fn can_edit_bucket_replication(
    pool: &SqlitePool,
    account_id: i64,
    bucket_id: i64,
) -> Result<bool> {
    if account_owns_bucket(pool, account_id, bucket_id).await? {
        return Ok(true);
    }
    check_perm(pool, account_id, bucket_id, CrudAction::Update).await
}

/// Ensure a bucket exists by name (used when receiving replicated objects).
pub async fn ensure_bucket(pool: &SqlitePool, name: &str) -> Result<i64> {
    if let Some(b) = get_bucket_by_name(pool, name).await? {
        return Ok(b.id);
    }
    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO bucket (name, replicate_to_all)
        VALUES (?1, 1)
        RETURNING id
        "#,
    )
    .bind(name)
    .fetch_one(pool)
    .await?;
    if let Some(owner) = get_owner_role(pool).await? {
        upsert_role_permission(pool, owner.id, id, CrudPerms::FULL).await?;
    }
    Ok(id)
}

/// Assign any buckets still without an owner to `account_id` (e.g. after bootstrap).
pub async fn claim_unowned_buckets(pool: &SqlitePool, account_id: i64) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE bucket
        SET owner_account_id = ?1
        WHERE owner_account_id IS NULL
        "#,
    )
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn effective_perms(
    pool: &SqlitePool,
    account_id: i64,
    bucket_id: i64,
) -> Result<CrudPerms> {
    if account_owns_bucket(pool, account_id, bucket_id).await? {
        return Ok(CrudPerms::FULL);
    }
    if account_is_owner(pool, account_id).await? {
        return Ok(CrudPerms::FULL);
    }
    let rows = sqlx::query_as::<_, RoleBucketPermission>(
        r#"
        SELECT rbp.*
        FROM role_bucket_permission rbp
        JOIN account_role ar ON ar.role_id = rbp.role_id
        WHERE ar.account_id = ?1 AND rbp.bucket_id = ?2
        "#,
    )
    .bind(account_id)
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;

    let mut acc = CrudPerms::NONE;
    for r in rows {
        acc = acc.or(CrudPerms {
            can_create: r.can_create,
            can_read: r.can_read,
            can_update: r.can_update,
            can_delete: r.can_delete,
        });
    }
    Ok(acc)
}

pub async fn check_perm(
    pool: &SqlitePool,
    account_id: i64,
    bucket_id: i64,
    action: CrudAction,
) -> Result<bool> {
    let p = effective_perms(pool, account_id, bucket_id).await?;
    Ok(match action {
        CrudAction::Create => p.can_create,
        CrudAction::Read => p.can_read,
        CrudAction::Update => p.can_update,
        CrudAction::Delete => p.can_delete,
    })
}

pub async fn create_session(
    pool: &SqlitePool,
    account_id: i64,
    ttl: Duration,
) -> Result<(String, DateTime<Utc>)> {
    let token = generate_session_token();
    let expires = Utc::now() + ttl;
    sqlx::query(
        r#"INSERT INTO session (token, account_id, expires_utc) VALUES (?1, ?2, ?3)"#,
    )
    .bind(&token)
    .bind(account_id)
    .bind(expires)
    .execute(pool)
    .await?;
    Ok((token, expires))
}

pub async fn delete_session(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query(r#"DELETE FROM session WHERE token = ?1"#)
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_sessions_for_account(pool: &SqlitePool, account_id: i64) -> Result<()> {
    sqlx::query(r#"DELETE FROM session WHERE account_id = ?1"#)
        .bind(account_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn resolve_session(pool: &SqlitePool, token: &str) -> Result<Option<AccountRecord>> {
    let row = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
        r#"SELECT account_id, expires_utc FROM session WHERE token = ?1"#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;

    let Some((account_id, expires)) = row else {
        return Ok(None);
    };
    if expires < Utc::now() {
        let _ = delete_session(pool, token).await;
        return Ok(None);
    }
    let account = get_account_by_id(pool, account_id).await?;
    if let Some(ref a) = account {
        if a.is_disabled {
            let _ = delete_session(pool, token).await;
            return Ok(None);
        }
    }
    Ok(account)
}

pub async fn list_buckets(pool: &SqlitePool) -> Result<Vec<BucketRecord>> {
    let rows = sqlx::query_as::<_, BucketRecord>(
        r#"SELECT * FROM bucket ORDER BY name ASC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_bucket_by_id(pool: &SqlitePool, id: i64) -> Result<Option<BucketRecord>> {
    let row = sqlx::query_as::<_, BucketRecord>(r#"SELECT * FROM bucket WHERE id = ?1"#)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn get_bucket_by_name(pool: &SqlitePool, name: &str) -> Result<Option<BucketRecord>> {
    let row = sqlx::query_as::<_, BucketRecord>(
        r#"SELECT * FROM bucket WHERE name = ?1 LIMIT 1"#,
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn create_bucket(
    pool: &SqlitePool,
    name: &str,
    owner_account_id: i64,
    etag_type: crate::db::models::EtagType,
) -> Result<BucketRecord> {
    let id = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO bucket (name, owner_account_id, etag_type)
        VALUES (?1, ?2, ?3)
        RETURNING id
        "#,
    )
    .bind(name)
    .bind(owner_account_id)
    .bind(etag_type.to_string())
    .fetch_one(pool)
    .await?;

    // Grant Owner role full CRUD on the new bucket (UI convenience; Owner also bypasses checks).
    if let Some(owner) = get_owner_role(pool).await? {
        upsert_role_permission(pool, owner.id, id, CrudPerms::FULL).await?;
    }

    get_bucket_by_id(pool, id)
        .await?
        .context("bucket missing after insert")
}

/// Object ids in a bucket (active and soft-deleted). Caller must purge before deleting the bucket.
pub async fn list_object_ids_in_bucket(pool: &SqlitePool, bucket_id: i64) -> Result<Vec<i64>> {
    let ids = sqlx::query_scalar::<_, i64>(
        r#"SELECT id FROM object WHERE bucket_id = ?1 ORDER BY id ASC"#,
    )
    .bind(bucket_id)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

pub async fn rename_bucket(pool: &SqlitePool, id: i64, new_name: &str) -> Result<BucketRecord> {
    let bucket = get_bucket_by_id(pool, id).await?.context("bucket not found")?;
    if bucket.name == "storage" {
        bail!("cannot rename default storage bucket");
    }
    if new_name == "storage" {
        bail!("cannot use reserved bucket name storage");
    }
    if new_name == bucket.name {
        return Ok(bucket);
    }
    let result = sqlx::query(r#"UPDATE bucket SET name = ?1 WHERE id = ?2"#)
        .bind(new_name)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("bucket not found");
    }
    get_bucket_by_id(pool, id)
        .await?
        .context("bucket missing after rename")
}

pub async fn set_bucket_owner(
    pool: &SqlitePool,
    id: i64,
    new_owner_id: i64,
) -> Result<BucketRecord> {
    let _bucket = get_bucket_by_id(pool, id).await?.context("bucket not found")?;
    let account = get_account_by_id(pool, new_owner_id)
        .await?
        .context("account not found")?;
    if account.is_disabled {
        bail!("cannot transfer ownership to a disabled account");
    }
    sqlx::query(r#"UPDATE bucket SET owner_account_id = ?1 WHERE id = ?2"#)
        .bind(new_owner_id)
        .bind(id)
        .execute(pool)
        .await?;
    get_bucket_by_id(pool, id)
        .await?
        .context("bucket missing after ownership transfer")
}

/// Enabled accounts for pickers (id + display name only).
pub async fn list_account_directory(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        r#"
        SELECT id, COALESCE(NULLIF(TRIM(display_name), ''), username_hex) AS label
        FROM account
        WHERE is_disabled = 0
        ORDER BY label ASC, id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_bucket(pool: &SqlitePool, id: i64) -> Result<()> {
    let bucket = get_bucket_by_id(pool, id).await?.context("bucket not found")?;
    if bucket.name == "storage" {
        bail!("cannot delete default storage bucket");
    }
    let remaining: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM object WHERE bucket_id = ?1"#,
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    if remaining > 0 {
        bail!("bucket still contains objects");
    }
    sqlx::query(r#"DELETE FROM bucket WHERE id = ?1"#)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_settings(pool: &SqlitePool) -> Result<AppSettings> {
    let row = sqlx::query_as::<_, AppSettings>(r#"SELECT * FROM app_settings WHERE id = 1"#)
        .fetch_optional(pool)
        .await?
        .context("app_settings missing")?;
    Ok(row)
}

pub async fn set_recycle_retention(
    pool: &SqlitePool,
    value: i64,
    unit: RetentionUnit,
) -> Result<AppSettings> {
    if value < 0 {
        bail!("retention value must be >= 0");
    }
    sqlx::query(
        r#"
        UPDATE app_settings
        SET recycle_retention_value = ?1, recycle_retention_unit = ?2
        WHERE id = 1
        "#,
    )
    .bind(value)
    .bind(unit.to_string())
    .execute(pool)
    .await?;
    get_settings(pool).await
}
