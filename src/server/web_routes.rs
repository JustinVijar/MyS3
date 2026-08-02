use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use qrcode::{Color, QrCode};
use rust_embed::Embed;
use serde::Deserialize;
use serde::Serialize;
use tokio_util::io::ReaderStream;

use chrono::{DateTime, Utc};

use crate::db::models::{CrudAction, ShareAccessMode, ShareTargetKind};
use crate::db::repository;
use crate::db::shares;
use crate::network::wireguard::{peer_config_snippet, WgSnapshot};
use crate::server::folder_archive::{
    build_archive, filter_archive_entries, folder_download_basename, ArchiveFormat, TempArchive,
};
use crate::server::keys::{normalize_folder_prefix, normalize_object_key};
use crate::server::media_access::{self, MediaGrant};
use crate::server::s3_routes::{
    delete_object_keyed_in_bucket, get_object_keyed_with_headers, put_object_keyed_in_bucket,
};
use crate::server::session_auth::{
    require_bucket_perm, resolve_bucket_id, AuthAccount,
};
use crate::AppState;

#[derive(Embed)]
#[folder = "web-ui/"]
struct Asset;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/stats", get(stats))
        .route("/api/v1/objects", get(objects))
        .route("/api/v1/objects/list", get(objects_list))
        .route("/api/v1/folders", axum::routing::delete(delete_folder))
        .route("/api/v1/folders/rename", post(rename_folder))
        .route("/api/v1/folders/archive", get(folder_archive))
        // axum 0.7 catch-all: /*key must be the final path segment.
        .route(
            "/api/v1/objects/content/*key",
            get(web_get_object_content),
        )
        .route(
            "/api/v1/objects/*key",
            put(web_put_object).delete(web_delete_object),
        )
        .route("/api/v1/media-links", post(create_media_link))
        .route(
            "/api/v1/media/content/*key",
            get(media_content_by_access),
        )
        .route("/api/v1/peers", get(peers))
        .route("/api/v1/wg/qr", get(wg_qr))
        .fallback(static_or_spa)
}

#[derive(Serialize)]
struct StatsResponse {
    bytes_stored: i64,
    object_count: i64,
    outbox_pending: i64,
    wireguard: WgSnapshot,
    node_id: String,
}

async fn stats(State(state): State<AppState>, _auth: AuthAccount) -> Response {
    match repository::stats(&state.db).await {
        Ok((bytes, count, pending)) => Json(StatsResponse {
            bytes_stored: bytes,
            object_count: count,
            outbox_pending: pending,
            wireguard: state.wg.status.snapshot(),
            node_id: state.config.node_id.clone(),
        })
        .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct BucketQuery {
    bucket: Option<String>,
}

async fn objects(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<BucketQuery>,
) -> Response {
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }
    match repository::list_objects_in_bucket(&state.db, bucket_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    prefix: String,
    #[serde(default = "default_delimiter")]
    delimiter: String,
    search: Option<String>,
    bucket: Option<String>,
    /// Skip this many folder+object entries (folders first).
    #[serde(default)]
    offset: i64,
    /// Max entries to return. `0` / omitted → default page size for lazy explorer.
    limit: Option<i64>,
}

fn default_delimiter() -> String {
    "/".to_string()
}

const DEFAULT_LIST_LIMIT: i64 = 50;

async fn objects_list(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<ListQuery>,
) -> Response {
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, 500);
    let offset = q.offset.max(0);
    match repository::list_objects_with_prefix(
        &state.db,
        &q.prefix,
        &q.delimiter,
        q.search.as_deref(),
        Some(bucket_id),
        offset,
        limit,
    )
    .await
    {
        Ok(result) => Json(result).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct FolderQuery {
    bucket: Option<String>,
    prefix: String,
}

#[derive(Serialize)]
struct FolderMutateResponse {
    affected: u64,
}

async fn delete_folder(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<FolderQuery>,
) -> Response {
    let prefix = match normalize_folder_prefix(&q.prefix) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Delete).await {
        return r;
    }
    match repository::soft_delete_prefix(&state.db, bucket_id, &prefix).await {
        Ok(affected) => Json(FolderMutateResponse { affected }).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct RenameFolderBody {
    bucket: Option<String>,
    from_prefix: String,
    to_prefix: String,
}

#[derive(Deserialize)]
struct FolderArchiveQuery {
    bucket: Option<String>,
    prefix: String,
    format: String,
}

async fn folder_archive(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<FolderArchiveQuery>,
) -> Response {
    let prefix = match normalize_folder_prefix(&q.prefix) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let format = match ArchiveFormat::parse(&q.format) {
        Ok(f) => f,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }

    let records = match repository::list_keys_under_prefix(&state.db, bucket_id, &prefix).await {
        Ok(rows) => rows,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let entries = filter_archive_entries(&state.engine, &prefix, &records);
    if entries.is_empty() {
        return (StatusCode::BAD_REQUEST, "folder is empty").into_response();
    }

    let out_path = match build_archive(&state.engine, format, entries).await {
        Ok(p) => p,
        Err(err) => {
            let msg = err.to_string();
            let status = if msg.contains("folder is empty") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return (status, msg).into_response();
        }
    };

    let archive = match TempArchive::open(out_path).await {
        Ok(a) => a,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let len = match archive.len().await {
        Ok(n) => n,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let basename = folder_download_basename(&prefix)
        .chars()
        .map(|c| if c == '"' || c == '\\' || c == '/' { '_' } else { c })
        .collect::<String>();
    let filename = format!("{basename}.{}", format.extension());
    let disposition = format!("attachment; filename=\"{filename}\"");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .unwrap_or(HeaderValue::from_static("attachment")),
    );

    let stream = ReaderStream::new(archive);
    (headers, Body::from_stream(stream)).into_response()
}

async fn rename_folder(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<RenameFolderBody>,
) -> Response {
    let from_prefix = match normalize_folder_prefix(&body.from_prefix) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, format!("from_prefix: {msg}")).into_response(),
    };
    let to_prefix = match normalize_folder_prefix(&body.to_prefix) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, format!("to_prefix: {msg}")).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, body.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Update).await {
        return r;
    }
    match repository::rename_prefix(&state.db, bucket_id, &from_prefix, &to_prefix).await {
        Ok(affected) => Json(FolderMutateResponse { affected }).into_response(),
        Err(err) => {
            let msg = err.to_string();
            let status = if msg.contains("already exists") || msg.contains("empty or does not") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, msg).into_response()
        }
    }
}

async fn web_put_object(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(raw_key): Path<String>,
    Query(q): Query<BucketQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };

    let existing =
        match repository::get_object_by_filename_in_bucket(&state.db, &key, bucket_id).await {
            Ok(v) => v,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };
    let action = if existing.is_some() {
        CrudAction::Update
    } else {
        CrudAction::Create
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, action).await {
        return r;
    }

    put_object_keyed_in_bucket(state, key, headers, body, Some(bucket_id)).await
}

async fn web_delete_object(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(raw_key): Path<String>,
    Query(q): Query<BucketQuery>,
) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Delete).await {
        return r;
    }
    delete_object_keyed_in_bucket(state, key, Some(bucket_id)).await
}

async fn web_get_object_content(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(raw_key): Path<String>,
    Query(q): Query<BucketQuery>,
    headers: HeaderMap,
) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let (bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }
    // Ensure object belongs to bucket (get_object_keyed looks up by filename only).
    match repository::get_object_by_filename_in_bucket(&state.db, &key, bucket_id).await {
        Ok(Some(_)) => get_object_keyed_with_headers(state, key, &headers).await,
        Ok(None) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/xml")],
            r#"<?xml version="1.0"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>"#,
        )
            .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct MediaLinkBody {
    bucket: Option<String>,
    key: String,
    kind: ShareTargetKind,
}

#[derive(Serialize)]
struct MediaLinkResponse {
    url: String,
    /// Relative path (for clients that prepend origin themselves).
    path: String,
    access_mode: Option<ShareAccessMode>,
    auth: &'static str,
    expires_at: Option<DateTime<Utc>>,
}

async fn create_media_link(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<MediaLinkBody>,
) -> Response {
    let (bucket_id, bucket_name) = match resolve_bucket_id(&state, body.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), bucket_id, CrudAction::Read).await {
        return r;
    }

    match body.kind {
        ShareTargetKind::Folder => {
            let prefix = match normalize_folder_prefix(&body.key) {
                Ok(p) => p,
                Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
            };
            let rows = match shares::list_folder_shares_for_prefix(&state.db, bucket_id, &prefix)
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
                }
            };
            let Some(share) = shares::prefer_share_for_link(&rows, None) else {
                return (
                    StatusCode::CONFLICT,
                    "create a share for this folder first",
                )
                    .into_response();
            };
            let mut path = media_access::share_page_path(share);
            let (auth_kind, expires_at) = if media_access::share_needs_access_token(share.access_mode)
            {
                match media_access::mint_share_token(&state.config, share) {
                    Ok((tok, exp)) => {
                        path = media_access::append_access_query(&path, &tok);
                        ("token", Some(exp))
                    }
                    Err(err) => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
                    }
                }
            } else {
                ("none", share.expires_at)
            };
            Json(MediaLinkResponse {
                url: path.clone(),
                path,
                access_mode: Some(share.access_mode),
                auth: auth_kind,
                expires_at,
            })
            .into_response()
        }
        ShareTargetKind::File => {
            let key = match normalize_object_key(&body.key) {
                Ok(k) => k,
                Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
            };
            if repository::get_object_by_filename_in_bucket(&state.db, &key, bucket_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                return (StatusCode::NOT_FOUND, "object not found").into_response();
            }
            let rows = match shares::list_shares_covering_key(&state.db, bucket_id, &key).await {
                Ok(r) => r,
                Err(err) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
                }
            };
            if let Some(share) = shares::prefer_share_for_link(&rows, Some(&key)) {
                let mut path = media_access::share_content_path(share, &key);
                let (auth_kind, expires_at) =
                    if media_access::share_needs_access_token(share.access_mode) {
                        match media_access::mint_share_token(&state.config, share) {
                            Ok((tok, exp)) => {
                                path = media_access::append_access_query(&path, &tok);
                                ("token", Some(exp))
                            }
                            Err(err) => {
                                return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response();
                            }
                        }
                    } else {
                        ("none", share.expires_at)
                    };
                return Json(MediaLinkResponse {
                    url: path.clone(),
                    path,
                    access_mode: Some(share.access_mode),
                    auth: auth_kind,
                    expires_at,
                })
                .into_response();
            }

            // Personal short-lived link.
            let (tok, exp) =
                match media_access::mint_personal_token(&state.config, auth.id(), bucket_id, &key) {
                    Ok(v) => v,
                    Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
                };
            let path = media_access::append_access_query(
                &media_access::personal_content_path(&bucket_name, &key),
                &tok,
            );
            Json(MediaLinkResponse {
                url: path.clone(),
                path,
                access_mode: None,
                auth: "token",
                expires_at: Some(exp),
            })
            .into_response()
        }
    }
}

#[derive(Deserialize)]
struct MediaContentQuery {
    bucket: Option<String>,
    access: String,
}

async fn media_content_by_access(
    State(state): State<AppState>,
    Path(raw_key): Path<String>,
    Query(q): Query<MediaContentQuery>,
    headers: HeaderMap,
) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let grant = match media_access::verify_access_token(&state.config, &q.access) {
        Ok(g) => g,
        Err(err) => return (StatusCode::UNAUTHORIZED, err).into_response(),
    };
    let MediaGrant::Personal {
        bucket_id,
        key: tok_key,
        ..
    } = grant
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "access token is not a personal media grant",
        )
            .into_response();
    };
    if tok_key != key {
        return (StatusCode::FORBIDDEN, "key mismatch").into_response();
    }
    let (resolved_bucket_id, _) = match resolve_bucket_id(&state, q.bucket.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if resolved_bucket_id != bucket_id {
        return (StatusCode::FORBIDDEN, "bucket mismatch").into_response();
    }
    match repository::get_object_by_filename_in_bucket(&state.db, &key, bucket_id).await {
        Ok(Some(_)) => get_object_keyed_with_headers(state, key, &headers).await,
        Ok(None) => (StatusCode::NOT_FOUND, "object not found").into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn peers(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = crate::server::session_auth::require_owner(&state, auth.id()).await {
        return r;
    }
    match repository::list_active_peers(&state.db).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct QrQuery {
    endpoint: Option<String>,
}

async fn wg_qr(State(state): State<AppState>, auth: AuthAccount, Query(q): Query<QrQuery>) -> Response {
    if let Err(r) = crate::server::session_auth::require_owner(&state, auth.id()).await {
        return r;
    }
    let endpoint = q
        .endpoint
        .unwrap_or_else(|| state.config.grpc_bind_addr.to_string());
    let snippet = peer_config_snippet(
        &state.config.node_id,
        &endpoint,
        "REPLACE_WITH_NODE_PUBLIC_KEY",
    );
    match qr_png_base64(&snippet) {
        Ok(b64) => Json(serde_json::json!({
            "config": snippet,
            "png_base64": b64,
        }))
        .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

fn qr_png_base64(data: &str) -> anyhow::Result<String> {
    let code = QrCode::new(data.as_bytes())?;
    let width = code.width();
    let scale = 8usize;
    let quiet = 4usize;
    let size = (width + quiet * 2) * scale;
    let mut img = image::GrayImage::from_pixel(size as u32, size as u32, image::Luma([255]));
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        img.put_pixel(
                            ((x + quiet) * scale + dx) as u32,
                            ((y + quiet) * scale + dy) as u32,
                            image::Luma([0]),
                        );
                    }
                }
            }
        }
    }
    let mut png = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png);
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut cursor, image::ImageFormat::Png)?;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

async fn static_or_spa(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Asset::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime)],
            file.data.to_vec(),
        )
            .into_response();
    }

    if let Some(index) = Asset::get("index.html") {
        return Html(String::from_utf8_lossy(&index.data).into_owned()).into_response();
    }

    (StatusCode::NOT_FOUND, "not found").into_response()
}
