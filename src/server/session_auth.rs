use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::db::models::{AccountRecord, CrudAction};
use crate::db::rbac;
use crate::AppState;

pub const SESSION_COOKIE: &str = "mys3_session";

#[derive(Debug, Clone)]
pub struct AuthAccount(pub AccountRecord);

impl AuthAccount {
    pub fn id(&self) -> i64 {
        self.0.id
    }
}

pub fn extract_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            let s = s.trim();
            if let Some(rest) = s.strip_prefix("Bearer ") {
                let t = rest.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Some(cookie) = headers.get(axum::http::header::COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
                    let v = v.trim();
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

#[async_trait]
impl FromRequestParts<AppState> for AuthAccount {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = extract_token_from_headers(&parts.headers).ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, "authentication required").into_response()
        })?;

        match rbac::resolve_session(&state.db, &token).await {
            Ok(Some(account)) => Ok(AuthAccount(account)),
            Ok(None) => Err((StatusCode::UNAUTHORIZED, "invalid or expired session").into_response()),
            Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
        }
    }
}

/// Optional auth — does not reject missing sessions.
pub struct OptionalAuth(pub Option<AccountRecord>);

#[async_trait]
impl FromRequestParts<AppState> for OptionalAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(token) = extract_token_from_headers(&parts.headers) else {
            return Ok(OptionalAuth(None));
        };
        match rbac::resolve_session(&state.db, &token).await {
            Ok(account) => Ok(OptionalAuth(account)),
            Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
        }
    }
}

pub async fn require_owner(state: &AppState, account_id: i64) -> Result<(), Response> {
    match rbac::account_is_owner(&state.db, account_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((StatusCode::FORBIDDEN, "owner role required").into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}

pub async fn require_bucket_perm(
    state: &AppState,
    account_id: i64,
    bucket_id: i64,
    action: CrudAction,
) -> Result<(), Response> {
    match rbac::check_perm(&state.db, account_id, bucket_id, action).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((StatusCode::FORBIDDEN, "insufficient permissions").into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}

pub async fn resolve_bucket_id(
    state: &AppState,
    bucket_name: Option<&str>,
) -> Result<(i64, String), Response> {
    let name = bucket_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("storage");
    match rbac::get_bucket_by_name(&state.db, name).await {
        Ok(Some(b)) => Ok((b.id, b.name)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("bucket '{name}' not found")).into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}
