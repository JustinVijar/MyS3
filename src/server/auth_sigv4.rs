use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

const AUTH_PREFIX: &str = "AWS4-HMAC-SHA256 Credential=";

/// SigV4 middleware for `/storage/objects/*`.
pub async fn require_sigv4(req: Request, next: Next) -> Response {
    let state = match req.extensions().get::<AppState>() {
        Some(s) => s.clone(),
        None => {
            return xml_sig_error("InternalError", "Missing application state");
        }
    };

    // Allow unsigned when explicitly requested for local smoke tests.
    if std::env::var("S3_AUTH_OPTIONAL").as_deref() == Ok("1") {
        return next.run(req).await;
    }

    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if auth.is_empty() {
        return xml_sig_error(
            "AccessDenied",
            "Authorization header missing; SigV4 required",
        );
    }

    match validate_sigv4(&req, &auth, &state) {
        Ok(()) => next.run(req).await,
        Err(msg) => {
            warn!("SigV4 rejection: {msg}");
            xml_sig_error("SignatureDoesNotMatch", &msg)
        }
    }
}

fn xml_sig_error(code: &str, message: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Error>
  <Code>{code}</Code>
  <Message>{message}</Message>
</Error>"#
    );
    (
        StatusCode::FORBIDDEN,
        [(header::CONTENT_TYPE, "application/xml")],
        body,
    )
        .into_response()
}

fn validate_sigv4(req: &Request, authorization: &str, state: &AppState) -> Result<(), String> {
    if !authorization.starts_with("AWS4-HMAC-SHA256 ") {
        return Err(
            "The request signature we calculated does not match the signature you provided."
                .into(),
        );
    }

    let parts = parse_authorization(authorization)?;
    if parts.access_key != state.config.aws_access_key_id {
        return Err(
            "The request signature we calculated does not match the signature you provided."
                .into(),
        );
    }

    let amz_date = header_str(req, "x-amz-date")
        .ok_or_else(|| "Missing x-amz-date header".to_string())?;
    let date_stamp = &amz_date[..8.min(amz_date.len())];
    let payload_hash = header_str(req, "x-amz-content-sha256")
        .unwrap_or_else(|| "UNSIGNED-PAYLOAD".to_string());

    let canonical_uri = req.uri().path().to_string();
    let canonical_query = req.uri().query().unwrap_or("");
    let signed_headers = parts.signed_headers.clone();
    let canonical_headers = build_canonical_headers(req, &signed_headers)?;

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method().as_str(),
        canonical_uri,
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let canonical_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, parts.region, parts.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date, scope, canonical_hash
    );

    let signing_key = derive_signing_key(
        &state.config.aws_secret_access_key,
        date_stamp,
        &parts.region,
        &parts.service,
    )?;
    let mut mac =
        HmacSha256::new_from_slice(&signing_key).map_err(|e| e.to_string())?;
    mac.update(string_to_sign.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    if !constant_time_eq(expected.as_bytes(), parts.signature.as_bytes()) {
        return Err(
            "The request signature we calculated does not match the signature you provided."
                .into(),
        );
    }
    Ok(())
}

struct AuthParts {
    access_key: String,
    region: String,
    service: String,
    signed_headers: String,
    signature: String,
}

fn parse_authorization(authorization: &str) -> Result<AuthParts, String> {
    // AWS4-HMAC-SHA256 Credential=AKID/date/region/service/aws4_request, SignedHeaders=..., Signature=...
    let rest = authorization
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or_else(|| "Invalid Authorization algorithm".to_string())?;

    let mut access_key = String::new();
    let mut region = String::from("us-east-1");
    let mut service = String::from("s3");
    let mut signed_headers = String::new();
    let mut signature = String::new();

    for part in rest.split(',') {
        let part = part.trim();
        if let Some(cred) = part.strip_prefix("Credential=") {
            let mut segs = cred.split('/');
            access_key = segs.next().unwrap_or("").to_string();
            let _date = segs.next();
            region = segs.next().unwrap_or("us-east-1").to_string();
            service = segs.next().unwrap_or("s3").to_string();
        } else if let Some(sh) = part.strip_prefix("SignedHeaders=") {
            signed_headers = sh.to_string();
        } else if let Some(sig) = part.strip_prefix("Signature=") {
            signature = sig.to_string();
        }
    }

    if access_key.is_empty() || signed_headers.is_empty() || signature.is_empty() {
        return Err("Malformed Authorization header".into());
    }

    // silence unused AUTH_PREFIX warning by referencing
    let _ = AUTH_PREFIX;
    Ok(AuthParts {
        access_key,
        region,
        service,
        signed_headers,
        signature,
    })
}

fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn build_canonical_headers(req: &Request, signed_headers: &str) -> Result<String, String> {
    let mut out = String::new();
    for name in signed_headers.split(';') {
        let key = name.trim().to_ascii_lowercase();
        let value = if key == "host" {
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        } else {
            header_str(req, &key).unwrap_or_default()
        };
        let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&key);
        out.push(':');
        out.push_str(&compact);
        out.push('\n');
    }
    Ok(out)
}

fn derive_signing_key(
    secret: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, String> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Helper kept for type presence when composing routers.
#[allow(dead_code)]
pub fn empty_body() -> Body {
    Body::empty()
}
