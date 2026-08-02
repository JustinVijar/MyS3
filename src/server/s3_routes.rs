use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tracing::error;

use crate::cluster::outbox;
use crate::db::models::EtagType;
use crate::db::repository;
use crate::server::keys::normalize_object_key;
use crate::tui::events::ServerEvent;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/storage/objects", get(list_objects))
        // axum 0.7 catch-all syntax is /*key (not {*key}, which is 0.8+)
        .route(
            "/storage/objects/*key",
            put(put_object).get(get_object).delete(delete_object),
        )
}

async fn list_objects(State(state): State<AppState>) -> Response {
    match repository::list_objects(&state.db).await {
        Ok(objects) => {
            let mut contents = String::new();
            for obj in objects {
                contents.push_str(&format!(
                    "<Contents><Key>{}</Key><Size>{}</Size><ETag>&quot;{}&quot;</ETag><LastModified>{}</LastModified></Contents>",
                    xml_escape(&obj.original_filename),
                    obj.filesize_bytes,
                    xml_escape(&obj.etag),
                    obj.date_modified.to_rfc3339()
                ));
            }
            let body = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>storage</Name>
  <IsTruncated>false</IsTruncated>
  {contents}
</ListBucketResult>"#
            );
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response()
        }
        Err(err) => {
            error!("list_objects: {err:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

async fn put_object(
    State(state): State<AppState>,
    Path(raw_key): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    put_object_keyed(state, key, headers, body).await
}

/// Shared PUT implementation used by S3 and open web API routes.
pub async fn put_object_keyed(
    state: AppState,
    key: String,
    headers: HeaderMap,
    body: Body,
) -> Response {
    put_object_keyed_in_bucket(state, key, headers, body, None).await
}

pub async fn put_object_keyed_in_bucket(
    state: AppState,
    key: String,
    headers: HeaderMap,
    body: Body,
    bucket_id: Option<i64>,
) -> Response {
    let bucket_id = match bucket_id {
        Some(id) => id,
        None => match repository::default_bucket_id(&state.db).await {
            Ok(id) => id,
            Err(err) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
            }
        },
    };

    let bucket_default = match repository::bucket_etag_type(&state.db, bucket_id).await {
        Ok(t) => t,
        Err(err) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    let etag_type = resolve_etag_type(&headers, bucket_default);

    let stream = body.into_data_stream();
    let mapped = futures::StreamExt::map(stream, |r| r.map_err(|e| anyhow::anyhow!(e)));

    let stored = match state
        .engine
        .put_chunks(mapped, &key, etag_type, None, None)
        .await
    {
        Ok(s) => s,
        Err(err) => {
            error!("put stream failed: {err:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            let _ = state.engine.unlink(&stored.filepath).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    let previous_size = match repository::get_object_by_filename_in_bucket(
        &state.db,
        &key,
        bucket_id,
    )
    .await
    {
        Ok(Some(existing)) => existing.filesize_bytes,
        Ok(None) => 0,
        Err(err) => {
            let _ = state.engine.unlink(&stored.filepath).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };
    let net_growth = stored.filesize_bytes.saturating_sub(previous_size);
    if let Err(err) = repository::check_hard_quota(&state.db, bucket_id, net_growth).await {
        let _ = state.engine.unlink(&stored.filepath).await;
        let msg = err.to_string();
        if msg.contains("quota exceeded") {
            return (StatusCode::PAYLOAD_TOO_LARGE, msg).into_response();
        }
        return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
    }

    // Soft-delete any active object with the same key so the partial unique index allows insert.
    if let Err(err) =
        repository::soft_delete_object_by_filename(&state.db, &key, Some(bucket_id)).await
    {
        let _ = state.engine.unlink(&stored.filepath).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    let obj_id = match repository::insert_object_tx(
        &mut tx,
        &key,
        &stored.filepath,
        &stored.file_format,
        stored.filesize_bytes,
        &stored.etag_type,
        &stored.etag,
        bucket_id,
    )
    .await
    {
        Ok(id) => id,
        Err(err) => {
            let _ = tx.rollback().await;
            let _ = state.engine.unlink(&stored.filepath).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    if let Err(err) = outbox::enqueue_put_tx(
        &mut tx,
        obj_id,
        &stored.filepath,
        &stored.etag,
        &state.config.node_id,
    )
    .await
    {
        let _ = tx.rollback().await;
        let _ = state.engine.unlink(&stored.filepath).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    if let Err(err) = tx.commit().await {
        let _ = state.engine.unlink(&stored.filepath).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    let _ = state.events.send(ServerEvent::BytesUploaded(
        stored.filesize_bytes as usize,
    ));
    let _ = state.events.send(ServerEvent::ObjectCreated {
        filename: key.clone(),
        size: stored.filesize_bytes,
    });

    etag_response(StatusCode::OK, &stored.etag, stored.etag_type, None)
}

async fn get_object(State(state): State<AppState>, Path(raw_key): Path<String>) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    get_object_keyed(state, key).await
}

/// Shared GET (stream download) used by S3 and open web API routes.
pub async fn get_object_keyed(state: AppState, key: String) -> Response {
    get_object_keyed_with_headers(state, key, &HeaderMap::new()).await
}

/// Parse a single-range `bytes=` header. Returns `(start, end_inclusive)`.
fn parse_bytes_range(range_val: &str, file_len: u64) -> Option<(u64, u64)> {
    let s = range_val.trim();
    let rest = s.strip_prefix("bytes=")?.trim();
    // Only support a single range (VLC/browsers typically send one).
    let unit = rest.split(',').next()?.trim();
    if file_len == 0 {
        return None;
    }
    if let Some(suffix) = unit.strip_prefix('-') {
        let n: u64 = suffix.parse().ok()?;
        if n == 0 {
            return None;
        }
        let n = n.min(file_len);
        return Some((file_len - n, file_len - 1));
    }
    let (start_s, end_s) = unit.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    if start >= file_len {
        return None;
    }
    let end = if end_s.is_empty() {
        file_len - 1
    } else {
        end_s.parse::<u64>().ok()?.min(file_len - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Like [`get_object_keyed`] but honors `Range` for 206 Partial Content (VLC seek).
pub async fn get_object_keyed_with_headers(
    state: AppState,
    key: String,
    req_headers: &HeaderMap,
) -> Response {
    let record = match repository::get_object_by_filename(&state.db, &key).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/xml")],
                r#"<?xml version="1.0"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>"#,
            )
                .into_response();
        }
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let file_len = record.filesize_bytes.max(0) as u64;
    let range_hdr = req_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let parsed_range = range_hdr.and_then(|r| parse_bytes_range(r, file_len));

    if range_hdr.is_some() && parsed_range.is_none() && file_len > 0 {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes */{file_len}"))
                .unwrap_or(HeaderValue::from_static("bytes */0")),
        );
        return (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response();
    }

    let mut file = match state.engine.open_read(&record.filepath).await {
        Ok(f) => f,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let mime = mime_guess::from_path(&record.original_filename)
        .first_or_octet_stream()
        .to_string();

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", record.etag)).unwrap_or(HeaderValue::from_static("\"\"")),
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if record.etag_type != EtagType::Md5 {
        headers.insert(
            "x-amz-meta-custom-etag",
            HeaderValue::from_str(&record.etag).unwrap_or(HeaderValue::from_static("")),
        );
        headers.insert(
            "x-amz-meta-etag-type",
            HeaderValue::from_str(&record.etag_type.to_string())
                .unwrap_or(HeaderValue::from_static("")),
        );
    }

    let (status, content_len) = if let Some((start, end)) = parsed_range {
        if let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
        let len = end - start + 1;
        let limited = file.take(len);
        let stream = ReaderStream::new(limited);
        let body = Body::from_stream(stream);
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&len.to_string()).unwrap_or(HeaderValue::from_static("0")),
        );
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{file_len}"))
                .unwrap_or(HeaderValue::from_static("bytes 0-0/0")),
        );
        let _ = state
            .events
            .send(ServerEvent::BytesDownloaded(len as usize));
        return (StatusCode::PARTIAL_CONTENT, headers, body).into_response();
    } else {
        (StatusCode::OK, file_len)
    };

    let _ = state
        .events
        .send(ServerEvent::BytesDownloaded(content_len as usize));

    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_len.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);
    (status, headers, body).into_response()
}

async fn delete_object(State(state): State<AppState>, Path(raw_key): Path<String>) -> Response {
    let key = match normalize_object_key(&raw_key) {
        Ok(k) => k,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    delete_object_keyed(state, key).await
}

/// Shared DELETE used by S3 and open web API routes.
/// Soft-deletes into the recycle bin; hard purge is handled by retention worker / recycle API.
pub async fn delete_object_keyed(state: AppState, key: String) -> Response {
    delete_object_keyed_in_bucket(state, key, None).await
}

pub async fn delete_object_keyed_in_bucket(
    state: AppState,
    key: String,
    bucket_id: Option<i64>,
) -> Response {
    match repository::soft_delete_object_by_filename(&state.db, &key, bucket_id).await {
        Ok(Some(_)) => StatusCode::NO_CONTENT.into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

fn resolve_etag_type(headers: &HeaderMap, default: EtagType) -> EtagType {
    headers
        .get("x-amz-meta-etag-type")
        .or_else(|| headers.get("x-etag-type"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn etag_response(
    status: StatusCode,
    etag: &str,
    etag_type: EtagType,
    body: Option<String>,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or(HeaderValue::from_static("\"\"")),
    );
    if etag_type != EtagType::Md5 {
        headers.insert(
            "x-amz-meta-custom-etag",
            HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("")),
        );
        headers.insert(
            "x-amz-meta-etag-type",
            HeaderValue::from_str(&etag_type.to_string()).unwrap_or(HeaderValue::from_static("")),
        );
    }
    match body {
        Some(b) => (status, headers, b).into_response(),
        None => (status, headers).into_response(),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
