//! One-shot password reset for local recovery.
//! Usage: cargo run --example reset_password -- <db_path> <account_id>

use anyhow::{bail, Context, Result};
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::Argon2;
use rand::RngCore;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::env;
use std::path::PathBuf;
use std::str::FromStr;

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash password: {e}"))?
        .to_string();
    Ok(hash)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let db_path = PathBuf::from(args.next().context("usage: reset_password <db_path> <account_id>")?);
    let account_id: i64 = args
        .next()
        .context("usage: reset_password <db_path> <account_id>")?
        .parse()
        .context("account_id must be an integer")?;

    if !db_path.exists() {
        bail!("database not found: {}", db_path.display());
    }

    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))?
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("connect sqlite")?;

    let row = sqlx::query_as::<_, (String, String)>(
        r#"SELECT username_hex, display_name FROM account WHERE id = ?1"#,
    )
    .bind(account_id)
    .fetch_optional(&pool)
    .await?
    .with_context(|| format!("no account with id {account_id}"))?;

    let password_hex = random_hex(32);
    let password_hash = hash_password(&password_hex)?;

    let updated = sqlx::query(
        r#"UPDATE account SET password_hash = ?1, updated_utc = CURRENT_TIMESTAMP WHERE id = ?2"#,
    )
    .bind(&password_hash)
    .bind(account_id)
    .execute(&pool)
    .await?
    .rows_affected();

    if updated != 1 {
        bail!("expected to update 1 row, updated {updated}");
    }

    println!("display_name={}", row.1);
    println!("username_hex={}", row.0);
    println!("password_hex={password_hex}");
    Ok(())
}
