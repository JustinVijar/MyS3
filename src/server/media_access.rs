//! Signed query `access` tokens for VLC/browser media links.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::Config;
use crate::db::models::{ShareAccessMode, ShareLinkRecord};

type HmacSha256 = Hmac<Sha256>;

const PERSONAL_TTL_SECS: i64 = 24 * 3600;
const SHARE_TOKEN_MAX_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaGrant {
    /// Authorizes any key in the share's scope until `exp`.
    Share { share_id: i64, bucket_id: i64, exp: i64 },
    /// Authorizes one object key until `exp`.
    Personal {
        account_id: i64,
        bucket_id: i64,
        key: String,
        exp: i64,
    },
}

impl MediaGrant {
    pub fn exp_unix(&self) -> i64 {
        match self {
            Self::Share { exp, .. } | Self::Personal { exp, .. } => *exp,
        }
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.exp_unix(), 0).unwrap_or_else(Utc::now)
    }
}

fn media_secret(config: &Config) -> Vec<u8> {
    let raw = std::env::var("MEDIA_LINK_SECRET").unwrap_or_else(|_| {
        format!("mys3-media-link:{}", config.aws_secret_access_key)
    });
    raw.into_bytes()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sign(secret: &[u8], payload: &str) -> Result<String, String> {
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|e| format!("hmac key error: {e}"))?;
    mac.update(payload.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn verify_sig(secret: &[u8], payload: &str, sig_hex: &str) -> bool {
    match sign(secret, payload) {
        Ok(expected) => {
            // Constant-time-ish compare
            expected.len() == sig_hex.len()
                && expected
                    .as_bytes()
                    .iter()
                    .zip(sig_hex.as_bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        }
        Err(_) => false,
    }
}

pub fn mint_share_token(config: &Config, share: &ShareLinkRecord) -> Result<(String, DateTime<Utc>), String> {
    let now = Utc::now();
    let max_exp = now + Duration::seconds(SHARE_TOKEN_MAX_SECS);
    let exp = match share.expires_at {
        Some(e) if e < max_exp => e,
        _ => max_exp,
    };
    if exp <= now {
        return Err("share already expired".into());
    }
    let exp_unix = exp.timestamp();
    let payload = format!("v1.s.{}.{}.{}", share.id, share.bucket_id, exp_unix);
    let sig = sign(&media_secret(config), &payload)?;
    Ok((format!("{payload}.{sig}"), exp))
}

pub fn mint_personal_token(
    config: &Config,
    account_id: i64,
    bucket_id: i64,
    key: &str,
) -> Result<(String, DateTime<Utc>), String> {
    let exp = Utc::now() + Duration::seconds(PERSONAL_TTL_SECS);
    let exp_unix = exp.timestamp();
    let key_b64 = URL_SAFE_NO_PAD.encode(key.as_bytes());
    let payload = format!("v1.p.{account_id}.{bucket_id}.{key_b64}.{exp_unix}");
    let sig = sign(&media_secret(config), &payload)?;
    Ok((format!("{payload}.{sig}"), exp))
}

pub fn verify_access_token(config: &Config, token: &str) -> Result<MediaGrant, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty access token".into());
    }
    let (payload, sig) = token
        .rsplit_once('.')
        .ok_or_else(|| "malformed access token".to_string())?;
    if !verify_sig(&media_secret(config), payload, sig) {
        return Err("invalid access token signature".into());
    }
    let parts: Vec<&str> = payload.split('.').collect();
    match parts.as_slice() {
        ["v1", "s", share_id, bucket_id, exp] => {
            let share_id: i64 = share_id.parse().map_err(|_| "bad share_id")?;
            let bucket_id: i64 = bucket_id.parse().map_err(|_| "bad bucket_id")?;
            let exp: i64 = exp.parse().map_err(|_| "bad exp")?;
            if exp < now_unix() {
                return Err("access token expired".into());
            }
            Ok(MediaGrant::Share {
                share_id,
                bucket_id,
                exp,
            })
        }
        ["v1", "p", account_id, bucket_id, key_b64, exp] => {
            let account_id: i64 = account_id.parse().map_err(|_| "bad account_id")?;
            let bucket_id: i64 = bucket_id.parse().map_err(|_| "bad bucket_id")?;
            let exp: i64 = exp.parse().map_err(|_| "bad exp")?;
            if exp < now_unix() {
                return Err("access token expired".into());
            }
            let key_bytes = URL_SAFE_NO_PAD
                .decode(key_b64.as_bytes())
                .map_err(|_| "bad key encoding")?;
            let key = String::from_utf8(key_bytes).map_err(|_| "bad key utf8")?;
            Ok(MediaGrant::Personal {
                account_id,
                bucket_id,
                key,
                exp,
            })
        }
        _ => Err("unknown access token format".into()),
    }
}

pub fn share_needs_access_token(mode: ShareAccessMode) -> bool {
    mode != ShareAccessMode::Public
}

/// Encode object key path segments for URL path (not query).
pub fn encode_key_path(key: &str) -> String {
    key.split('/')
        .map(|p| urlencoding_encode(p))
        .collect::<Vec<_>>()
        .join("/")
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn share_content_path(share: &ShareLinkRecord, key: &str) -> String {
    let id_part = if let Some(code) = share.short_code.as_deref() {
        format!("/api/v1/shares/by-code/{code}/content/{}", encode_key_path(key))
    } else {
        format!(
            "/api/v1/shares/by-token/{}/content/{}",
            share.token,
            encode_key_path(key)
        )
    };
    id_part
}

pub fn share_page_path(share: &ShareLinkRecord) -> String {
    crate::db::shares::share_url_path(share)
}

pub fn personal_content_path(bucket: &str, key: &str) -> String {
    format!(
        "/api/v1/media/content/{}?bucket={}",
        encode_key_path(key),
        urlencoding_encode(bucket)
    )
}

pub fn append_access_query(path_and_maybe_query: &str, access: &str) -> String {
    let sep = if path_and_maybe_query.contains('?') {
        '&'
    } else {
        '?'
    };
    format!(
        "{path_and_maybe_query}{sep}access={}",
        urlencoding_encode(access)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::ShareTargetKind;
    use std::net::SocketAddr;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            storage_root: PathBuf::from(".data"),
            bind_addr: "127.0.0.1:9000".parse::<SocketAddr>().unwrap(),
            grpc_bind_addr: "127.0.0.1:50051".parse::<SocketAddr>().unwrap(),
            embed_wg: false,
            node_id: "n1".into(),
            aws_access_key_id: "k".into(),
            aws_secret_access_key: "test-secret".into(),
            default_etag_type: crate::db::models::EtagType::Md5,
            cluster_peers: vec![],
            wg_private_key: None,
            disable_tui: true,
        }
    }

    #[test]
    fn roundtrip_personal() {
        let cfg = test_config();
        let (tok, _) = mint_personal_token(&cfg, 1, 2, "a/b.mp4").unwrap();
        let g = verify_access_token(&cfg, &tok).unwrap();
        match g {
            MediaGrant::Personal {
                account_id,
                bucket_id,
                key,
                ..
            } => {
                assert_eq!(account_id, 1);
                assert_eq!(bucket_id, 2);
                assert_eq!(key, "a/b.mp4");
            }
            _ => panic!("expected personal"),
        }
    }

    #[test]
    fn roundtrip_share() {
        let cfg = test_config();
        let share = ShareLinkRecord {
            id: 9,
            token: "t".into(),
            short_code: None,
            bucket_id: 3,
            target_key: "f/".into(),
            target_kind: ShareTargetKind::Folder,
            access_mode: ShareAccessMode::BucketReaders,
            expires_at: None,
            created_by_account_id: 1,
            created_at: Utc::now(),
            revoked_at: None,
        };
        let (tok, _) = mint_share_token(&cfg, &share).unwrap();
        let g = verify_access_token(&cfg, &tok).unwrap();
        match g {
            MediaGrant::Share {
                share_id,
                bucket_id,
                ..
            } => {
                assert_eq!(share_id, 9);
                assert_eq!(bucket_id, 3);
            }
            _ => panic!("expected share"),
        }
    }
}
