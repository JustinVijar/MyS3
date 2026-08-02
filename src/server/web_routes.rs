use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use qrcode::{Color, QrCode};
use rust_embed::Embed;
use serde::Deserialize;
use serde::Serialize;

use crate::db::models::CrudAction;
use crate::db::repository;
use crate::network::wireguard::{peer_config_snippet, WgSnapshot};
use crate::server::keys::{normalize_folder_prefix, normalize_object_key};
use crate::server::s3_routes::{
    delete_object_keyed_in_bucket, get_object_keyed, put_object_keyed_in_bucket,
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
        // axum 0.7 catch-all: /*key must be the final path segment.
        .route(
            "/api/v1/objects/content/*key",
            get(web_get_object_content),
        )
        .route(
            "/api/v1/objects/*key",
            put(web_put_object).delete(web_delete_object),
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
}

fn default_delimiter() -> String {
    "/".to_string()
}

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
    match repository::list_objects_with_prefix(
        &state.db,
        &q.prefix,
        &q.delimiter,
        q.search.as_deref(),
        Some(bucket_id),
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
        Ok(Some(_)) => get_object_keyed(state, key).await,
        Ok(None) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/xml")],
            r#"<?xml version="1.0"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>"#,
        )
            .into_response(),
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
