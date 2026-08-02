pub mod auth_sigv4;
pub mod etag_rehash;
pub mod ffmpeg;
pub mod folder_archive;
pub mod keys;
pub mod purge;
pub mod s3_routes;
pub mod session_auth;
pub mod settings_routes;
pub mod share_routes;
pub mod web_routes;

use axum::middleware;
use axum::Router;

use crate::AppState;

pub fn build_router(state: AppState) -> Router {
    let s3 = s3_routes::router().layer(middleware::from_fn_with_state(
        state.clone(),
        auth_sigv4_mw,
    ));

    Router::new()
        .merge(s3)
        .merge(settings_routes::router())
        .merge(share_routes::router())
        .merge(web_routes::router())
        .with_state(state)
}

async fn auth_sigv4_mw(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    req.extensions_mut().insert(state);
    auth_sigv4::require_sigv4(req, next).await
}
