//! Stream browser-playable video previews via system ffmpeg.

use std::path::Path;
use std::process::Stdio;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::process::Command;
use tokio_util::io::ReaderStream;
use tracing::warn;

/// Allowed downscale heights for preview streaming.
pub const ALLOWED_HEIGHTS: &[u32] = &[720, 480, 320, 114];

/// True if `ffmpeg` is on PATH and responds to `-version`.
pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parse `height` query: omit / `0` / `original` => None (no scale).
/// Allowed numeric values: 720, 480, 320, 114.
pub fn parse_preview_height(raw: Option<&str>) -> Result<Option<u32>, &'static str> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if raw.eq_ignore_ascii_case("original") || raw == "0" {
        return Ok(None);
    }
    let h: u32 = raw
        .parse()
        .map_err(|_| "invalid height (use original, 720, 480, 320, or 114)")?;
    if ALLOWED_HEIGHTS.contains(&h) {
        Ok(Some(h))
    } else {
        Err("invalid height (use original, 720, 480, 320, or 114)")
    }
}

/// Transcode/remux `input_path` to fragmented H.264/AAC MP4 on stdout.
/// When `height` is Some, downscale to that max height without upscaling.
pub async fn stream_preview_mp4(input_path: &Path, height: Option<u32>) -> Response {
    if !input_path.is_file() {
        return (StatusCode::NOT_FOUND, "object file missing").into_response();
    }

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(input_path);

    if let Some(h) = height {
        // Keep aspect ratio, even width; never upscale above source height.
        let vf = format!("scale=-2:'min(ih,{h})'");
        cmd.arg("-vf").arg(vf);
    }

    cmd.args([
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-c:a",
        "aac",
        "-ac",
        "2",
        "-movflags",
        "frag_keyframe+empty_moov",
        "-f",
        "mp4",
        "pipe:1",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ffmpeg not installed on server",
                )
                    .into_response();
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "ffmpeg stdout missing").into_response();
        }
    };

    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if !status.success() => {
                warn!("ffmpeg preview exited with {status}");
            }
            Err(err) => warn!("ffmpeg wait error: {err}"),
            _ => {}
        }
    });

    let stream = ReaderStream::new(stdout);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Body::from_stream(stream)).into_response()
}
