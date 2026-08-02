use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};

use crate::db::models::{CrudAction, ShareAccessMode, ShareLinkRecord, ShareTargetKind};
use crate::db::repository;
use crate::db::{rbac, shares};
use crate::server::keys::{normalize_folder_prefix, normalize_object_key};
use crate::server::media_access::{self, MediaGrant};
use crate::server::s3_routes::get_object_keyed_with_headers;
use crate::server::session_auth::{
    require_bucket_perm, resolve_bucket_id, AuthAccount, OptionalAuth,
};
use crate::AppState;

#[derive(Embed)]
#[folder = "web-ui/"]
struct ShareAsset;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/shares", post(create_share).get(list_shares))
        .route("/api/v1/shares/:id", delete(revoke_share))
        .route(
            "/api/v1/shares/by-token/:token/content/*key",
            get(share_content_by_token),
        )
        .route(
            "/api/v1/shares/by-code/:code/content/*key",
            get(share_content_by_code),
        )
        .route(
            "/api/v1/shares/by-token/:token/list",
            get(share_list_by_token),
        )
        .route(
            "/api/v1/shares/by-code/:code/list",
            get(share_list_by_code),
        )
        .route(
            "/api/v1/shares/by-token/:token",
            get(share_meta_by_token),
        )
        .route("/api/v1/shares/by-code/:code", get(share_meta_by_code))
        .route("/share/:token", get(share_page_token))
        .route("/s/:code", get(share_page_code))
}

async fn share_page_token() -> Response {
    serve_share_html()
}

async fn share_page_code() -> Response {
    serve_share_html()
}

fn serve_share_html() -> Response {
    match ShareAsset::get("share.html") {
        Some(file) => Html(String::from_utf8_lossy(&file.data).into_owned()).into_response(),
        None => (StatusCode::NOT_FOUND, "share page not found").into_response(),
    }
}

#[derive(Deserialize)]
struct CreateShareBody {
    bucket: Option<String>,
    key: String,
    kind: ShareTargetKind,
    access_mode: ShareAccessMode,
    #[serde(default)]
    account_ids: Vec<i64>,
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    shorten: bool,
}

#[derive(Serialize)]
struct ShareView {
    id: i64,
    token: String,
    short_code: Option<String>,
    url_path: String,
    bucket_id: i64,
    target_key: String,
    target_kind: ShareTargetKind,
    access_mode: ShareAccessMode,
    expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    account_ids: Vec<i64>,
}

fn to_view(share: ShareLinkRecord, account_ids: Vec<i64>) -> ShareView {
    let url_path = shares::share_url_path(&share);
    ShareView {
        id: share.id,
        token: share.token,
        short_code: share.short_code,
        url_path,
        bucket_id: share.bucket_id,
        target_key: share.target_key,
        target_kind: share.target_kind,
        access_mode: share.access_mode,
        expires_at: share.expires_at,
        created_at: share.created_at,
        account_ids,
    }
}

async fn create_share(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<CreateShareBody>,
) -> Response {
    let (bucket_id, _) = match resolve_bucket_id(&state, body.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }

    let target_key = match body.kind {
        ShareTargetKind::File => match normalize_object_key(&body.key) {
            Ok(k) => k,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        },
        ShareTargetKind::Folder => match normalize_folder_prefix(&body.key) {
            Ok(k) => k,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        },
    };

    if let Some(exp) = body.expires_at {
        if exp <= Utc::now() {
            return (StatusCode::BAD_REQUEST, "expires_at must be in the future").into_response();
        }
    }

    match shares::create_share(
        &state.db,
        bucket_id,
        &target_key,
        body.kind,
        body.access_mode,
        body.expires_at,
        auth.id(),
        &body.account_ids,
        body.shorten,
    )
    .await
    {
        Ok(share) => {
            let recipients = shares::list_share_recipients(&state.db, share.id)
                .await
                .unwrap_or_default();
            (StatusCode::CREATED, Json(to_view(share, recipients))).into_response()
        }
        Err(err) => {
            let msg = err.to_string();
            let status = if msg.contains("not found") || msg.contains("empty") {
                StatusCode::NOT_FOUND
            } else if msg.contains("requires") || msg.contains("specific_users") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, msg).into_response()
        }
    }
}

#[derive(Deserialize)]
struct ListSharesQuery {
    bucket: Option<String>,
    key: String,
}

async fn list_shares(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<ListSharesQuery>,
) -> Response {
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }

    // Accept file key or folder prefix as stored.
    let key = q.key.trim().trim_start_matches('/');
    if key.is_empty() {
        return (StatusCode::BAD_REQUEST, "key is required").into_response();
    }

    match shares::list_shares_for_target(&state.db, bucket_id, key, auth.id()).await {
        Ok(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for share in rows {
                // Skip expired in list for UX (still not revoked).
                if shares::share_is_usable(&share, Utc::now()).is_err() {
                    continue;
                }
                let recipients = shares::list_share_recipients(&state.db, share.id)
                    .await
                    .unwrap_or_default();
                out.push(to_view(share, recipients));
            }
            Json(out).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn revoke_share(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    let is_owner = match rbac::account_is_owner(&state.db, auth.id()).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    // Ensure actor can at least read the bucket of the share (or is owner).
    let share = match shares::get_share_by_id(&state.db, id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "share not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if !is_owner {
        if let Err(r) =
            require_bucket_perm(&state, auth.id(), share.bucket_id, CrudAction::Read).await
        {
            return r;
        }
    }

    match shares::revoke_share(&state.db, id, auth.id(), is_owner).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "share not found").into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not allowed") {
                (StatusCode::FORBIDDEN, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

#[derive(Serialize)]
struct ShareMetaResponse {
    target_kind: ShareTargetKind,
    target_key: String,
    display_name: String,
    access_mode: ShareAccessMode,
    expires_at: Option<DateTime<Utc>>,
    login_required: bool,
    bucket_name: Option<String>,
    filesize_bytes: Option<i64>,
    content_type_hint: Option<String>,
}

fn display_name_for(share: &ShareLinkRecord) -> String {
    match share.target_kind {
        ShareTargetKind::File => share
            .target_key
            .rsplit('/')
            .next()
            .unwrap_or(&share.target_key)
            .to_string(),
        ShareTargetKind::Folder => {
            let trimmed = share.target_key.trim_end_matches('/');
            trimmed
                .rsplit('/')
                .next()
                .unwrap_or(trimmed)
                .to_string()
        }
    }
}

fn access_token_grants_share(state: &AppState, share: &ShareLinkRecord, access: Option<&str>) -> bool {
    let Some(raw) = access.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    match media_access::verify_access_token(&state.config, raw) {
        Ok(MediaGrant::Share {
            share_id,
            bucket_id,
            ..
        }) => share_id == share.id && bucket_id == share.bucket_id,
        _ => false,
    }
}

async fn authorize_share(
    state: &AppState,
    share: &ShareLinkRecord,
    auth: &OptionalAuth,
    access: Option<&str>,
) -> Result<(), Response> {
    if let Err(reason) = shares::share_is_usable(share, Utc::now()) {
        let status = match reason {
            shares::ShareDenyReason::Expired => StatusCode::GONE,
            shares::ShareDenyReason::Revoked => StatusCode::GONE,
        };
        return Err((status, "share is no longer available").into_response());
    }

    match share.access_mode {
        ShareAccessMode::Public => Ok(()),
        ShareAccessMode::BucketReaders => {
            if access_token_grants_share(state, share, access) {
                return Ok(());
            }
            let Some(account) = auth.0.as_ref() else {
                return Err((StatusCode::UNAUTHORIZED, "authentication required").into_response());
            };
            match rbac::check_perm(&state.db, account.id, share.bucket_id, CrudAction::Read).await {
                Ok(true) => Ok(()),
                Ok(false) => {
                    Err((StatusCode::FORBIDDEN, "insufficient permissions").into_response())
                }
                Err(err) => {
                    Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())
                }
            }
        }
        ShareAccessMode::SpecificUsers => {
            if access_token_grants_share(state, share, access) {
                return Ok(());
            }
            let Some(account) = auth.0.as_ref() else {
                return Err((StatusCode::UNAUTHORIZED, "authentication required").into_response());
            };
            match shares::share_allows_account(&state.db, share.id, account.id).await {
                Ok(true) => Ok(()),
                Ok(false) => Err((StatusCode::FORBIDDEN, "not on share recipient list").into_response()),
                Err(err) => {
                    Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())
                }
            }
        }
    }
}

#[derive(Deserialize, Default)]
struct AccessQuery {
    access: Option<String>,
}

async fn load_share_by_token(state: &AppState, token: &str) -> Result<ShareLinkRecord, Response> {
    match shares::get_share_by_token(&state.db, token).await {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err((StatusCode::NOT_FOUND, "share not found").into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}

async fn load_share_by_code(state: &AppState, code: &str) -> Result<ShareLinkRecord, Response> {
    match shares::get_share_by_short_code(&state.db, code).await {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err((StatusCode::NOT_FOUND, "share not found").into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}

async fn meta_for_share(
    state: &AppState,
    share: ShareLinkRecord,
    auth: OptionalAuth,
    access: Option<&str>,
) -> Response {
    if let Err(r) = authorize_share(state, &share, &auth, access).await {
        // For metadata, still tell the client if login is required (401) vs gone/forbidden.
        return r;
    }
    let bucket_name = match rbac::get_bucket_by_id(&state.db, share.bucket_id).await {
        Ok(Some(b)) => Some(b.name),
        _ => None,
    };
    let mut filesize_bytes = None;
    let mut content_type_hint = None;
    if share.target_kind == ShareTargetKind::File {
        if let Ok(Some(obj)) = repository::get_object_by_filename_in_bucket(
            &state.db,
            &share.target_key,
            share.bucket_id,
        )
        .await
        {
            filesize_bytes = Some(obj.filesize_bytes);
            if !obj.file_format.is_empty() {
                content_type_hint = Some(obj.file_format);
            }
        }
    }
    Json(ShareMetaResponse {
        target_kind: share.target_kind,
        target_key: share.target_key.clone(),
        display_name: display_name_for(&share),
        access_mode: share.access_mode,
        expires_at: share.expires_at,
        login_required: share.access_mode != ShareAccessMode::Public,
        bucket_name,
        filesize_bytes,
        content_type_hint,
    })
    .into_response()
}

async fn share_meta_by_token(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path(token): Path<String>,
    Query(aq): Query<AccessQuery>,
) -> Response {
    let share = match load_share_by_token(&state, &token).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let access = aq.access.as_deref();
    // Soft probe: if unauthorized, include login_required hint via JSON on 401.
    if share.access_mode != ShareAccessMode::Public
        && auth.0.is_none()
        && !access_token_grants_share(&state, &share, access)
    {
        if let Err(reason) = shares::share_is_usable(&share, Utc::now()) {
            let status = match reason {
                shares::ShareDenyReason::Expired | shares::ShareDenyReason::Revoked => {
                    StatusCode::GONE
                }
            };
            return (status, "share is no longer available").into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "authentication required",
                "login_required": true,
                "display_name": display_name_for(&share),
                "target_kind": share.target_kind,
                "access_mode": share.access_mode,
            })),
        )
            .into_response();
    }
    meta_for_share(&state, share, auth, access).await
}

async fn share_meta_by_code(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path(code): Path<String>,
    Query(aq): Query<AccessQuery>,
) -> Response {
    let share = match load_share_by_code(&state, &code).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let access = aq.access.as_deref();
    if share.access_mode != ShareAccessMode::Public
        && auth.0.is_none()
        && !access_token_grants_share(&state, &share, access)
    {
        if let Err(reason) = shares::share_is_usable(&share, Utc::now()) {
            let status = match reason {
                shares::ShareDenyReason::Expired | shares::ShareDenyReason::Revoked => {
                    StatusCode::GONE
                }
            };
            return (status, "share is no longer available").into_response();
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "authentication required",
                "login_required": true,
                "display_name": display_name_for(&share),
                "target_kind": share.target_kind,
                "access_mode": share.access_mode,
            })),
        )
            .into_response();
    }
    meta_for_share(&state, share, auth, access).await
}

#[derive(Deserialize)]
struct ShareListQuery {
    /// Absolute key prefix, or path relative to the share root.
    #[serde(default)]
    prefix: String,
    access: Option<String>,
}

async fn list_for_share(
    state: &AppState,
    share: ShareLinkRecord,
    auth: OptionalAuth,
    q: ShareListQuery,
) -> Response {
    if let Err(r) = authorize_share(state, &share, &auth, q.access.as_deref()).await {
        return r;
    }
    if share.target_kind != ShareTargetKind::Folder {
        return (StatusCode::BAD_REQUEST, "share is not a folder").into_response();
    }

    let list_prefix = if q.prefix.is_empty() {
        share.target_key.clone()
    } else if q.prefix.starts_with(&share.target_key) {
        // Absolute under share root.
        match normalize_folder_prefix(&q.prefix) {
            Ok(p) if p.starts_with(&share.target_key) => p,
            Ok(_) => {
                return (StatusCode::BAD_REQUEST, "prefix outside share scope").into_response();
            }
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    } else {
        match shares::resolve_list_prefix(&share, Some(&q.prefix)) {
            Ok(p) => p,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    };

    match repository::list_objects_with_prefix(
        &state.db,
        &list_prefix,
        "/",
        None,
        Some(share.bucket_id),
        0,
        0, // no page limit for share listings
    )
    .await
    {
        Ok(mut result) => {
            // Hide .keep markers from share UI.
            result.objects.retain(|o| !o.original_filename.ends_with(".keep"));
            Json(result).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn share_list_by_token(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path(token): Path<String>,
    Query(q): Query<ShareListQuery>,
) -> Response {
    let share = match load_share_by_token(&state, &token).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    list_for_share(&state, share, auth, q).await
}

async fn share_list_by_code(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path(code): Path<String>,
    Query(q): Query<ShareListQuery>,
) -> Response {
    let share = match load_share_by_code(&state, &code).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    list_for_share(&state, share, auth, q).await
}

async fn content_for_share(
    state: AppState,
    share: ShareLinkRecord,
    auth: OptionalAuth,
    raw_key: String,
    access: Option<&str>,
    headers: &HeaderMap,
) -> Response {
    if let Err(r) = authorize_share(&state, &share, &auth, access).await {
        return r;
    }
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    if !shares::key_in_share_scope(&share, &key) {
        return (StatusCode::FORBIDDEN, "key outside share scope").into_response();
    }
    match repository::get_object_by_filename_in_bucket(&state.db, &key, share.bucket_id).await {
        Ok(Some(_)) => get_object_keyed_with_headers(state, key, headers).await,
        Ok(None) => (StatusCode::NOT_FOUND, "object not found").into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn share_content_by_token(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path((token, raw_key)): Path<(String, String)>,
    Query(aq): Query<AccessQuery>,
    headers: HeaderMap,
) -> Response {
    let share = match load_share_by_token(&state, &token).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    content_for_share(state, share, auth, raw_key, aq.access.as_deref(), &headers).await
}

async fn share_content_by_code(
    State(state): State<AppState>,
    auth: OptionalAuth,
    Path((code, raw_key)): Path<(String, String)>,
    Query(aq): Query<AccessQuery>,
    headers: HeaderMap,
) -> Response {
    let share = match load_share_by_code(&state, &code).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    content_for_share(state, share, auth, raw_key, aq.access.as_deref(), &headers).await
}

