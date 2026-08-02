use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::cluster::outbox;
use crate::config::{persist_storage_root, resolve_storage_path};
use crate::db::models::{CrudAction, CrudPerms, EtagType, QuotaMode, RetentionUnit};
use crate::db::rbac;
use crate::db::repository;
use crate::db::repository::DEFAULT_NODE_ALLOCATED_BYTES;
use crate::server::etag_rehash;
use crate::server::session_auth::{
    require_bucket_perm, require_owner, resolve_bucket_id, AuthAccount, SESSION_COOKIE,
};
use crate::storage::reconcile;
use crate::storage::relocate;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/bootstrap", post(auth_bootstrap))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/buckets", get(list_buckets).post(create_bucket))
        .route(
            "/api/v1/buckets/:id",
            axum::routing::patch(patch_bucket).delete(delete_bucket),
        )
        .route(
            "/api/v1/buckets/:id/replication",
            get(get_bucket_replication).put(put_bucket_replication),
        )
        .route(
            "/api/v1/buckets/:id/nodes",
            get(list_bucket_nodes).post(add_bucket_node),
        )
        .route(
            "/api/v1/buckets/:id/nodes/:node_id",
            axum::routing::patch(patch_bucket_node).delete(delete_bucket_node),
        )
        .route(
            "/api/v1/buckets/:id/etag",
            axum::routing::put(put_bucket_etag),
        )
        .route(
            "/api/v1/accounts",
            get(list_accounts).post(create_account),
        )
        .route("/api/v1/accounts/directory", get(list_account_directory))
        .route(
            "/api/v1/accounts/:id",
            axum::routing::patch(patch_account).delete(delete_account),
        )
        .route(
            "/api/v1/accounts/:id/regenerate-password",
            post(regenerate_password),
        )
        .route("/api/v1/accounts/:id/roles", axum::routing::put(set_roles))
        .route("/api/v1/roles", get(list_roles).post(create_role))
        .route(
            "/api/v1/roles/:id",
            axum::routing::patch(patch_role).delete(delete_role),
        )
        .route(
            "/api/v1/roles/:id/permissions",
            get(get_role_permissions).put(put_role_permissions),
        )
        .route(
            "/api/v1/settings/storage",
            get(get_storage_settings).put(put_storage_settings),
        )
        .route(
            "/api/v1/settings/storage/integrity",
            get(get_storage_integrity),
        )
        .route(
            "/api/v1/settings/storage/integrity/reconcile",
            post(post_storage_reconcile),
        )
        .route(
            "/api/v1/settings/recycle",
            get(get_recycle_settings).put(put_recycle_settings),
        )
        .route("/api/v1/recycle-bin", get(list_recycle_bin))
        .route("/api/v1/recycle-bin/purge", post(purge_recycle_items))
        .route("/api/v1/recycle-bin/purge-all", post(purge_all_recycle_items))
        .route(
            "/api/v1/recycle-bin/:id/restore",
            post(restore_recycle_item),
        )
        .route(
            "/api/v1/recycle-bin/:id",
            axum::routing::delete(hard_delete_recycle_item),
        )
}

#[derive(Serialize)]
struct AuthStatusResponse {
    needs_bootstrap: bool,
    authenticated: bool,
    account: Option<AccountView>,
    is_owner: bool,
}

#[derive(Serialize)]
struct AccountView {
    id: i64,
    username_hex: String,
    display_name: String,
    is_disabled: bool,
    role_ids: Vec<i64>,
    created_utc: chrono::DateTime<chrono::Utc>,
    created_by_account_id: Option<i64>,
}

#[derive(Serialize)]
struct CredentialsOnce {
    username_hex: String,
    password_hex: String,
}

#[derive(Deserialize)]
struct BootstrapBody {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct LoginBody {
    username_hex: String,
    password_hex: String,
}

async fn auth_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let needs_bootstrap = match rbac::account_count(&state.db).await {
        Ok(n) => n == 0,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let token = crate::server::session_auth::extract_token_from_headers(&headers);
    let account = if let Some(t) = token {
        match rbac::resolve_session(&state.db, &t).await {
            Ok(a) => a,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        }
    } else {
        None
    };

    let (authenticated, account_view, is_owner) = if let Some(a) = account {
        let role_ids = match rbac::list_account_role_ids(&state.db, a.id).await {
            Ok(r) => r,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };
        let is_owner = match rbac::account_is_owner(&state.db, a.id).await {
            Ok(v) => v,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };
        (
            true,
            Some(AccountView {
                id: a.id,
                username_hex: a.username_hex,
                display_name: a.display_name,
                is_disabled: a.is_disabled,
                role_ids,
                created_utc: a.created_utc,
                created_by_account_id: a.created_by_account_id,
            }),
            is_owner,
        )
    } else {
        (false, None, false)
    };

    Json(AuthStatusResponse {
        needs_bootstrap,
        authenticated,
        account: account_view,
        is_owner,
    })
    .into_response()
}

async fn auth_bootstrap(State(state): State<AppState>, Json(body): Json<BootstrapBody>) -> Response {
    match rbac::account_count(&state.db).await {
        Ok(0) => {}
        Ok(_) => {
            return (StatusCode::CONFLICT, "bootstrap already completed").into_response();
        }
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }

    let (username_hex, password_hex) = rbac::generate_credentials();
    let display_name = body
        .display_name
        .unwrap_or_else(|| "Owner".to_string());

    let account = match rbac::create_account(
        &state.db,
        &username_hex,
        &password_hex,
        &display_name,
        None,
    )
    .await
    {
        Ok(a) => a,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let owner = match rbac::get_owner_role(&state.db).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Owner role missing").into_response();
        }
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    if let Err(err) = rbac::assign_role(&state.db, account.id, owner.id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    // Seeded buckets (e.g. storage) may have no owner yet before the first account exists.
    if let Err(err) = rbac::claim_unowned_buckets(&state.db, account.id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    let (token, expires) =
        match rbac::create_session(&state.db, account.id, Duration::days(7)).await {
            Ok(t) => t,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };

    let cookie = session_cookie(&token, expires);
    (
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({
            "account_id": account.id,
            "credentials": CredentialsOnce { username_hex, password_hex },
            "session_token": token,
        })),
    )
        .into_response()
}

async fn auth_login(State(state): State<AppState>, Json(body): Json<LoginBody>) -> Response {
    let account = match rbac::get_account_by_username(&state.db, body.username_hex.trim()).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if account.is_disabled {
        return (StatusCode::FORBIDDEN, "account disabled").into_response();
    }
    match rbac::verify_password(body.password_hex.trim(), &account.password_hash) {
        Ok(true) => {}
        Ok(false) => return (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }

    let (token, expires) =
        match rbac::create_session(&state.db, account.id, Duration::days(7)).await {
            Ok(t) => t,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };

    let cookie = session_cookie(&token, expires);
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({
            "account_id": account.id,
            "session_token": token,
        })),
    )
        .into_response()
}

async fn auth_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = crate::server::session_auth::extract_token_from_headers(&headers) {
        let _ = rbac::delete_session(&state.db, &token).await;
    }
    let clear = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
    );
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, clear)]).into_response()
}

fn session_cookie(token: &str, expires: chrono::DateTime<chrono::Utc>) -> String {
    let max_age = (expires - chrono::Utc::now()).num_seconds().max(0);
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    )
}

#[derive(Serialize)]
struct BucketListItem {
    id: i64,
    name: String,
    created_utc: chrono::DateTime<chrono::Utc>,
    owner_account_id: Option<i64>,
    replicate_to_all: bool,
    can_edit_replication: bool,
    etag_type: EtagType,
    etag_rehash_status: Option<String>,
    etag_rehash_processed: i64,
    etag_rehash_total: i64,
    etag_rehash_error: Option<String>,
}

async fn list_buckets(State(state): State<AppState>, auth: AuthAccount) -> Response {
    match rbac::list_buckets(&state.db).await {
        Ok(buckets) => {
            // Filter to buckets the account can read (Owner sees all).
            let mut out = Vec::new();
            for b in buckets {
                match rbac::check_perm(&state.db, auth.id(), b.id, CrudAction::Read).await {
                    Ok(true) => {
                        let can_edit_replication = match rbac::can_edit_bucket_replication(
                            &state.db,
                            auth.id(),
                            b.id,
                        )
                        .await
                        {
                            Ok(v) => v,
                            Err(err) => {
                                return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                                    .into_response();
                            }
                        };
                        out.push(BucketListItem {
                            id: b.id,
                            name: b.name,
                            created_utc: b.created_utc,
                            owner_account_id: b.owner_account_id,
                            replicate_to_all: b.replicate_to_all,
                            can_edit_replication,
                            etag_type: b.etag_type,
                            etag_rehash_status: b.etag_rehash_status,
                            etag_rehash_processed: b.etag_rehash_processed,
                            etag_rehash_total: b.etag_rehash_total,
                            etag_rehash_error: b.etag_rehash_error,
                        });
                    }
                    Ok(false) => {}
                    Err(err) => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
                    }
                }
            }
            Json(out).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct CreateBucketBody {
    name: String,
}

#[derive(Deserialize)]
struct PatchBucketBody {
    name: Option<String>,
    owner_account_id: Option<i64>,
}

fn validate_bucket_name(name: &str) -> Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err("invalid bucket name");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("bucket name may only contain alphanumeric, -, _, .");
    }
    Ok(name)
}

async fn create_bucket(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<CreateBucketBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let name = match validate_bucket_name(&body.name) {
        Ok(n) => n,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    match rbac::create_bucket(
        &state.db,
        name,
        auth.id(),
        state.config.default_etag_type,
    )
    .await
    {
        Ok(b) => {
            if let Err(err) = repository::assign_bucket_node(
                &state.db,
                b.id,
                &state.config.node_id,
                DEFAULT_NODE_ALLOCATED_BYTES,
                QuotaMode::Soft,
            )
            .await
            {
                return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
            }
            (StatusCode::CREATED, Json(b)).into_response()
        }
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("UNIQUE") {
                (StatusCode::CONFLICT, "bucket already exists").into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn patch_bucket(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PatchBucketBody>,
) -> Response {
    if body.name.is_none() && body.owner_account_id.is_none() {
        return (StatusCode::BAD_REQUEST, "no fields to update").into_response();
    }
    let owns = match rbac::account_owns_bucket(&state.db, auth.id(), id).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if !owns {
        return (
            StatusCode::FORBIDDEN,
            "only the bucket owner can rename or transfer this bucket",
        )
            .into_response();
    }

    let mut bucket = match rbac::get_bucket_by_id(&state.db, id).await {
        Ok(Some(b)) => b,
        Ok(None) => return (StatusCode::NOT_FOUND, "bucket not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    if let Some(raw_name) = body.name.as_deref() {
        let name = match validate_bucket_name(raw_name) {
            Ok(n) => n,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        };
        match rbac::rename_bucket(&state.db, id, name).await {
            Ok(b) => bucket = b,
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("UNIQUE") {
                    return (StatusCode::CONFLICT, "bucket already exists").into_response();
                }
                if msg.contains("cannot rename") || msg.contains("reserved") {
                    return (StatusCode::CONFLICT, msg).into_response();
                }
                if msg.contains("not found") {
                    return (StatusCode::NOT_FOUND, msg).into_response();
                }
                return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
            }
        }
    }

    if let Some(new_owner_id) = body.owner_account_id {
        match rbac::set_bucket_owner(&state.db, id, new_owner_id).await {
            Ok(b) => bucket = b,
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("account not found") {
                    return (StatusCode::NOT_FOUND, msg).into_response();
                }
                if msg.contains("disabled") {
                    return (StatusCode::CONFLICT, msg).into_response();
                }
                if msg.contains("bucket not found") {
                    return (StatusCode::NOT_FOUND, msg).into_response();
                }
                return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
            }
        }
    }

    Json(bucket).into_response()
}

#[derive(Serialize)]
struct PeerDirItem {
    id: String,
    endpoint: String,
}

#[derive(Serialize)]
struct BucketReplicationView {
    replicate_to_all: bool,
    peer_ids: Vec<String>,
    peers: Vec<PeerDirItem>,
}

#[derive(Deserialize)]
struct PutBucketReplicationBody {
    replicate_to_all: bool,
    #[serde(default)]
    peer_ids: Vec<String>,
}

async fn require_replication_editor(
    state: &AppState,
    account_id: i64,
    bucket_id: i64,
) -> Result<(), Response> {
    match rbac::can_edit_bucket_replication(&state.db, account_id, bucket_id).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::FORBIDDEN,
            "bucket owner or edit permission required",
        )
            .into_response()),
        Err(err) => Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    }
}

async fn get_bucket_replication(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    let (replicate_to_all, peer_ids) = match repository::get_bucket_replication(&state.db, id).await
    {
        Ok(v) => v,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                return (StatusCode::NOT_FOUND, msg).into_response();
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
        }
    };
    let peers = match repository::list_active_peers(&state.db).await {
        Ok(rows) => rows
            .into_iter()
            .map(|p| PeerDirItem {
                id: p.id,
                endpoint: p.wireguard_endpoint,
            })
            .collect(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    Json(BucketReplicationView {
        replicate_to_all,
        peer_ids,
        peers,
    })
    .into_response()
}

async fn put_bucket_replication(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PutBucketReplicationBody>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    match repository::set_bucket_replication(
        &state.db,
        id,
        body.replicate_to_all,
        &body.peer_ids,
    )
    .await
    {
        Ok(()) => {
            // Keep local node assigned after legacy replication edits.
            let _ = repository::ensure_bucket_node_assignment(
                &state.db,
                id,
                &state.config.node_id,
                DEFAULT_NODE_ALLOCATED_BYTES,
                QuotaMode::Soft,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

#[derive(Serialize)]
struct BucketNodeItem {
    id: String,
    endpoint: String,
    is_local: bool,
    allocated_bytes: i64,
    quota_mode: QuotaMode,
    used_bytes: i64,
}

#[derive(Serialize)]
struct BucketNodesView {
    nodes: Vec<BucketNodeItem>,
    available: Vec<PeerDirItem>,
    used_bytes: i64,
    etag_type: EtagType,
    etag_rehash_status: Option<String>,
    etag_rehash_processed: i64,
    etag_rehash_total: i64,
    etag_rehash_error: Option<String>,
    local_storage_path: String,
}

#[derive(Deserialize)]
struct PutBucketEtagBody {
    etag_type: EtagType,
    /// `new_only` | `recalculate_all`
    apply: String,
}

#[derive(Serialize)]
struct BucketEtagView {
    etag_type: EtagType,
    etag_rehash_status: Option<String>,
    etag_rehash_processed: i64,
    etag_rehash_total: i64,
    etag_rehash_error: Option<String>,
}

#[derive(Deserialize)]
struct AddBucketNodeBody {
    /// Existing peer id, or optional override when registering via `endpoint`.
    #[serde(default)]
    node_id: Option<String>,
    /// Peer gRPC endpoint URL (`host:port` or `http(s)://host:port`).
    #[serde(default)]
    endpoint: Option<String>,
    /// Allocation in whole GiB (converted to bytes server-side).
    allocated_gb: f64,
    #[serde(default = "default_quota_mode")]
    quota_mode: QuotaMode,
}

fn default_quota_mode() -> QuotaMode {
    QuotaMode::Soft
}

/// Parse a pasted peer URL into `(host:port, suggested_node_id)`.
fn parse_peer_endpoint(raw: &str) -> Result<(String, String), &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("endpoint is required");
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .or_else(|| trimmed.strip_prefix("grpc://"))
        .unwrap_or(trimmed);
    let hostport = without_scheme.split('/').next().unwrap_or("").trim();
    if hostport.is_empty() {
        return Err("invalid peer endpoint");
    }
    // Bracketed IPv6: [addr]:port or [addr]
    let (host, port) = if hostport.starts_with('[') {
        let end = hostport.find(']').ok_or("invalid IPv6 endpoint")?;
        let host = &hostport[..=end];
        let rest = &hostport[end + 1..];
        let port = if let Some(p) = rest.strip_prefix(':') {
            if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                return Err("invalid peer port");
            }
            p
        } else if rest.is_empty() {
            "50051"
        } else {
            return Err("invalid IPv6 endpoint");
        };
        (host.to_string(), port.to_string())
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        if h.is_empty() || p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err("invalid peer endpoint");
        }
        (h.to_string(), p.to_string())
    } else {
        (hostport.to_string(), "50051".to_string())
    };
    if host.is_empty() {
        return Err("invalid peer host");
    }
    let endpoint = format!("{host}:{port}");
    let host_for_id = host.trim_matches(|c| c == '[' || c == ']');
    let suggested_id = format!(
        "node-{}",
        host_for_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_ascii_lowercase()
    );
    if suggested_id == "node-" || suggested_id.is_empty() {
        return Err("could not derive node id from endpoint");
    }
    Ok((endpoint, suggested_id))
}

#[derive(Deserialize)]
struct PatchBucketNodeBody {
    allocated_gb: Option<f64>,
    quota_mode: Option<QuotaMode>,
}

fn gb_to_bytes(gb: f64) -> Result<i64, &'static str> {
    if !gb.is_finite() || gb <= 0.0 {
        return Err("allocated_gb must be a positive number");
    }
    let bytes = (gb * 1024.0 * 1024.0 * 1024.0).round();
    if bytes > i64::MAX as f64 {
        return Err("allocated_gb is too large");
    }
    Ok(bytes as i64)
}

async fn list_bucket_nodes(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    // Ensure local node is present for the UI.
    let _ = repository::ensure_bucket_node_assignment(
        &state.db,
        id,
        &state.config.node_id,
        DEFAULT_NODE_ALLOCATED_BYTES,
        QuotaMode::Soft,
    )
    .await;

    let used_bytes = match repository::bucket_used_bytes(&state.db, id).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let assignments = match repository::list_bucket_node_assignments(&state.db, id).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let peers = match repository::list_active_peers(&state.db).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let peer_map: std::collections::HashMap<_, _> = peers
        .iter()
        .map(|p| (p.id.clone(), p.wireguard_endpoint.clone()))
        .collect();
    let assigned_ids: std::collections::HashSet<_> =
        assignments.iter().map(|a| a.peer_id.clone()).collect();

    let nodes: Vec<BucketNodeItem> = assignments
        .into_iter()
        .map(|a| BucketNodeItem {
            is_local: a.peer_id == state.config.node_id,
            endpoint: peer_map
                .get(&a.peer_id)
                .cloned()
                .unwrap_or_default(),
            id: a.peer_id,
            allocated_bytes: a.allocated_bytes,
            quota_mode: a.quota_mode,
            used_bytes,
        })
        .collect();

    let available: Vec<PeerDirItem> = peers
        .into_iter()
        .filter(|p| !assigned_ids.contains(&p.id))
        .map(|p| PeerDirItem {
            id: p.id,
            endpoint: p.wireguard_endpoint,
        })
        .collect();

    let bucket = match rbac::get_bucket_by_id(&state.db, id).await {
        Ok(Some(b)) => b,
        Ok(None) => return (StatusCode::NOT_FOUND, "bucket not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let local_storage_path = relocate::absolute_display(&state.config.storage_root)
        .display()
        .to_string();

    Json(BucketNodesView {
        nodes,
        available,
        used_bytes,
        etag_type: bucket.etag_type,
        etag_rehash_status: bucket.etag_rehash_status,
        etag_rehash_processed: bucket.etag_rehash_processed,
        etag_rehash_total: bucket.etag_rehash_total,
        etag_rehash_error: bucket.etag_rehash_error,
        local_storage_path,
    })
    .into_response()
}

#[derive(Serialize)]
struct StorageSettingsView {
    path: String,
    absolute_path: String,
    /// Active DB objects (includes `.keep` markers).
    object_count: i64,
    /// Alias for UI: active DB object count including `.keep`.
    db_object_count: i64,
    /// Files currently under `objects/` on disk.
    disk_file_count: i64,
    used_bytes: i64,
    has_data: bool,
    node_id: String,
}

#[derive(Deserialize)]
struct PutStorageBody {
    path: String,
    /// `move` | `fresh`
    mode: String,
}

#[derive(Serialize)]
struct StorageChangeResponse {
    restart_required: bool,
    absolute_path: String,
    mode: String,
}

async fn get_storage_settings(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let (used_bytes, object_count, _) = match repository::stats(&state.db).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let disk_file_count = match reconcile::count_disk_files(state.engine.objects_dir()).await {
        Ok(n) => n,
        Err(_) => 0,
    };
    let objects_nonempty = disk_file_count > 0;
    let absolute = relocate::absolute_display(&state.config.storage_root);
    Json(StorageSettingsView {
        path: state.config.storage_root.display().to_string(),
        absolute_path: absolute.display().to_string(),
        object_count,
        db_object_count: object_count,
        disk_file_count,
        used_bytes,
        has_data: object_count > 0 || used_bytes > 0 || objects_nonempty,
        node_id: state.config.node_id.clone(),
    })
    .into_response()
}

async fn get_storage_integrity(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match reconcile::inspect(&state.db, &state.engine).await {
        Ok(report) => Json(report).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn post_storage_reconcile(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match reconcile::reconcile(&state.db, &state.engine).await {
        Ok(report) => Json(report).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn put_storage_settings(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<PutStorageBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let mode = body.mode.trim();
    if mode != "move" && mode != "fresh" {
        return (StatusCode::BAD_REQUEST, "mode must be move or fresh").into_response();
    }
    let new_path = match resolve_storage_path(&body.path) {
        Ok(p) => p,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    let current = relocate::absolute_display(&state.config.storage_root);
    if new_path == current {
        return StatusCode::NO_CONTENT.into_response();
    }

    let (used_bytes, object_count, _) = match repository::stats(&state.db).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let objects_nonempty = match relocate::dir_nonempty(state.engine.objects_dir()).await {
        Ok(v) => v,
        Err(_) => false,
    };
    let has_data = object_count > 0 || used_bytes > 0 || objects_nonempty;

    if mode == "move" {
        if let Ok(true) = relocate::dir_nonempty(&new_path).await {
            return (
                StatusCode::CONFLICT,
                "destination already exists and is not empty",
            )
                .into_response();
        }
        // Close DB before moving the storage root (metadata.db lives inside it).
        state.db.close().await;
        if let Err(err) = relocate::relocate_storage_root(&current, &new_path).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    } else {
        // Start fresh: leave old root untouched; create empty layout at destination.
        if let Err(err) = relocate::ensure_storage_layout(&new_path).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
        let _ = has_data; // client already confirmed when data exists
    }

    if let Err(err) = persist_storage_root(&new_path) {
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    // Give the response time to flush, then exit so the next start loads the new path.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        std::process::exit(0);
    });

    (
        StatusCode::ACCEPTED,
        Json(StorageChangeResponse {
            restart_required: true,
            absolute_path: new_path.display().to_string(),
            mode: mode.to_string(),
        }),
    )
        .into_response()
}

async fn put_bucket_etag(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PutBucketEtagBody>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    let apply = body.apply.trim();
    if apply != "new_only" && apply != "recalculate_all" {
        return (
            StatusCode::BAD_REQUEST,
            "apply must be new_only or recalculate_all",
        )
            .into_response();
    }

    let bucket = match rbac::get_bucket_by_id(&state.db, id).await {
        Ok(Some(b)) => b,
        Ok(None) => return (StatusCode::NOT_FOUND, "bucket not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    if apply == "recalculate_all"
        && bucket.etag_rehash_status.as_deref() == Some("running")
    {
        return (
            StatusCode::CONFLICT,
            "etag recalculation already running",
        )
            .into_response();
    }

    if let Err(err) = repository::set_bucket_etag_type(&state.db, id, body.etag_type).await {
        let msg = err.to_string();
        if msg.contains("not found") {
            return (StatusCode::NOT_FOUND, msg).into_response();
        }
        return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response();
    }

    if apply == "new_only" {
        return StatusCode::NO_CONTENT.into_response();
    }

    let total = match repository::count_active_objects_in_bucket(&state.db, id).await {
        Ok(n) => n,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if let Err(err) = repository::begin_bucket_etag_rehash(&state.db, id, total).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    etag_rehash::spawn_bucket_rehash(state.clone(), id, body.etag_type);

    let view = BucketEtagView {
        etag_type: body.etag_type,
        etag_rehash_status: Some("running".into()),
        etag_rehash_processed: 0,
        etag_rehash_total: total,
        etag_rehash_error: None,
    };
    (StatusCode::ACCEPTED, Json(view)).into_response()
}

async fn add_bucket_node(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<AddBucketNodeBody>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    let allocated_bytes = match gb_to_bytes(body.allocated_gb) {
        Ok(b) => b,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    let endpoint_raw = body
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let explicit_id = body
        .node_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let node_id = if let Some(endpoint_raw) = endpoint_raw {
        let (endpoint, suggested_id) = match parse_peer_endpoint(endpoint_raw) {
            Ok(v) => v,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        };
        let node_id = explicit_id.unwrap_or(suggested_id.as_str()).to_string();
        if node_id == state.config.node_id {
            return (
                StatusCode::BAD_REQUEST,
                "cannot add the local node via peer URL",
            )
                .into_response();
        }
        let local_ep = state.config.grpc_bind_addr.to_string();
        if endpoint == local_ep
            || endpoint == format!("127.0.0.1:{}", state.config.grpc_bind_addr.port())
            || endpoint == format!("localhost:{}", state.config.grpc_bind_addr.port())
        {
            return (
                StatusCode::BAD_REQUEST,
                "endpoint refers to this node",
            )
                .into_response();
        }
        if let Err(err) = repository::upsert_peer(&state.db, &node_id, &endpoint).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
        node_id
    } else {
        let Some(node_id) = explicit_id else {
            return (
                StatusCode::BAD_REQUEST,
                "endpoint or node_id is required",
            )
                .into_response();
        };
        if node_id == state.config.node_id {
            return (
                StatusCode::BAD_REQUEST,
                "cannot add the local node again",
            )
                .into_response();
        }
        node_id.to_string()
    };

    match repository::assign_bucket_node(
        &state.db,
        id,
        &node_id,
        allocated_bytes,
        body.quota_mode,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg).into_response()
            } else if msg.contains("must be") {
                (StatusCode::BAD_REQUEST, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn patch_bucket_node(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path((id, node_id)): Path<(i64, String)>,
    Json(body): Json<PatchBucketNodeBody>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    let allocated_bytes = match body.allocated_gb {
        Some(gb) => match gb_to_bytes(gb) {
            Ok(b) => Some(b),
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        },
        None => None,
    };
    match repository::update_bucket_node(
        &state.db,
        id,
        &node_id,
        allocated_bytes,
        body.quota_mode,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg).into_response()
            } else if msg.contains("must be") || msg.contains("no fields") {
                (StatusCode::BAD_REQUEST, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn delete_bucket_node(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path((id, node_id)): Path<(i64, String)>,
) -> Response {
    if let Err(r) = require_replication_editor(&state, auth.id(), id).await {
        return r;
    }
    match repository::remove_bucket_node(&state.db, id, &node_id, &state.config.node_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg).into_response()
            } else if msg.contains("cannot remove") {
                (StatusCode::CONFLICT, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn list_account_directory(State(state): State<AppState>, _auth: AuthAccount) -> Response {
    match rbac::list_account_directory(&state.db).await {
        Ok(rows) => {
            let out: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|(id, display_name)| {
                    serde_json::json!({
                        "id": id,
                        "display_name": display_name,
                    })
                })
                .collect();
            Json(out).into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn delete_bucket(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    let bucket = match rbac::get_bucket_by_id(&state.db, id).await {
        Ok(b) => b,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let owns = match rbac::account_owns_bucket(&state.db, auth.id(), id).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if !owns {
        return (
            StatusCode::FORBIDDEN,
            "only the bucket owner can delete this bucket",
        )
            .into_response();
    }
    if bucket.as_ref().map(|b| b.name.as_str()) == Some("storage") {
        return (
            StatusCode::CONFLICT,
            "cannot delete default storage bucket",
        )
            .into_response();
    }

    // Permanently purge all objects in the bucket (active + recycle) so FK allows DROP.
    let object_ids = match rbac::list_object_ids_in_bucket(&state.db, id).await {
        Ok(ids) => ids,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    for oid in object_ids {
        if let Err(err) = hard_purge_object(&state, oid).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    }

    match rbac::delete_bucket(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg).into_response()
            } else if msg.contains("cannot delete") || msg.contains("still contains") {
                (StatusCode::CONFLICT, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn list_accounts(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let accounts = match rbac::list_accounts(&state.db).await {
        Ok(a) => a,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let mut views = Vec::new();
    for a in accounts {
        let role_ids = match rbac::list_account_role_ids(&state.db, a.id).await {
            Ok(r) => r,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };
        views.push(AccountView {
            id: a.id,
            username_hex: a.username_hex,
            display_name: a.display_name,
            is_disabled: a.is_disabled,
            role_ids,
            created_utc: a.created_utc,
            created_by_account_id: a.created_by_account_id,
        });
    }
    Json(views).into_response()
}

#[derive(Deserialize)]
struct CreateAccountBody {
    display_name: Option<String>,
    role_ids: Option<Vec<i64>>,
}

async fn create_account(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<CreateAccountBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let (username_hex, password_hex) = rbac::generate_credentials();
    let display_name = body.display_name.unwrap_or_default();
    let account = match rbac::create_account(
        &state.db,
        &username_hex,
        &password_hex,
        &display_name,
        Some(auth.id()),
    )
    .await
    {
        Ok(a) => a,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if let Some(roles) = body.role_ids {
        if let Err(err) = rbac::set_account_roles(&state.db, account.id, &roles).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "account": {
                "id": account.id,
                "username_hex": account.username_hex,
                "display_name": account.display_name,
                "is_disabled": account.is_disabled,
                "created_by_account_id": account.created_by_account_id,
            },
            "credentials": CredentialsOnce { username_hex, password_hex },
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PatchAccountBody {
    display_name: Option<String>,
    is_disabled: Option<bool>,
}

async fn patch_account(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PatchAccountBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    if let Some(name) = body.display_name {
        if let Err(err) = rbac::update_account_display_name(&state.db, id, &name).await {
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    }
    if let Some(disabled) = body.is_disabled {
        if disabled && id == auth.id() {
            return (StatusCode::BAD_REQUEST, "cannot disable your own account").into_response();
        }
        if let Err(err) = rbac::set_account_disabled(&state.db, id, disabled).await {
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
        if disabled {
            let _ = rbac::delete_sessions_for_account(&state.db, id).await;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn delete_account(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if id == auth.id() {
        return (StatusCode::BAD_REQUEST, "cannot delete your own account").into_response();
    }
    let created_by_me = match rbac::account_created_by(&state.db, id, auth.id()).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if !created_by_me {
        return (
            StatusCode::FORBIDDEN,
            "only the creator of an account can delete it",
        )
            .into_response();
    }
    match rbac::delete_account(&state.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "account not found").into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn regenerate_password(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let account = match rbac::get_account_by_id(&state.db, id).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::NOT_FOUND, "account not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    let password_hex = rbac::random_hex(32);
    if let Err(err) = rbac::set_account_password(&state.db, id, &password_hex).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }
    let _ = rbac::delete_sessions_for_account(&state.db, id).await;
    Json(serde_json::json!({
        "credentials": CredentialsOnce {
            username_hex: account.username_hex,
            password_hex,
        }
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SetRolesBody {
    role_ids: Vec<i64>,
}

async fn set_roles(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<SetRolesBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    if rbac::get_account_by_id(&state.db, id).await.ok().flatten().is_none() {
        return (StatusCode::NOT_FOUND, "account not found").into_response();
    }
    match rbac::set_account_roles(&state.db, id, &body.role_ids).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn list_roles(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::list_roles(&state.db).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct CreateRoleBody {
    name: String,
    position: Option<i64>,
}

async fn create_role(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<CreateRoleBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let name = body.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name required").into_response();
    }
    let position = body.position.unwrap_or(0);
    match rbac::create_role(&state.db, name, position).await {
        Ok(r) => (StatusCode::CREATED, Json(r)).into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("UNIQUE") {
                (StatusCode::CONFLICT, "role already exists").into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

#[derive(Deserialize)]
struct PatchRoleBody {
    name: Option<String>,
    position: Option<i64>,
}

async fn patch_role(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PatchRoleBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::update_role(
        &state.db,
        id,
        body.name.as_deref().map(str::trim),
        body.position,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    }
}

async fn delete_role(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::delete_role(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("cannot delete") || msg.contains("not found") {
                (StatusCode::BAD_REQUEST, msg).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

async fn get_role_permissions(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::list_role_permissions(&state.db, id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct PermEntry {
    bucket_id: i64,
    can_create: bool,
    can_read: bool,
    can_update: bool,
    can_delete: bool,
}

#[derive(Deserialize)]
struct PutPermsBody {
    permissions: Vec<PermEntry>,
}

async fn put_role_permissions(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
    Json(body): Json<PutPermsBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    let role = match rbac::get_role(&state.db, id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "role not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if role.is_owner {
        // Still allow storing grants for UI, but Owner bypasses checks anyway.
    }
    let perms: Vec<(i64, CrudPerms)> = body
        .permissions
        .into_iter()
        .map(|p| {
            (
                p.bucket_id,
                CrudPerms {
                    can_create: p.can_create,
                    can_read: p.can_read,
                    can_update: p.can_update,
                    can_delete: p.can_delete,
                },
            )
        })
        .collect();
    match rbac::replace_role_permissions(&state.db, id, &perms).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn get_recycle_settings(State(state): State<AppState>, auth: AuthAccount) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::get_settings(&state.db).await {
        Ok(s) => Json(s).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct RecycleSettingsBody {
    recycle_retention_value: i64,
    recycle_retention_unit: RetentionUnit,
}

async fn put_recycle_settings(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<RecycleSettingsBody>,
) -> Response {
    if let Err(r) = require_owner(&state, auth.id()).await {
        return r;
    }
    match rbac::set_recycle_retention(
        &state.db,
        body.recycle_retention_value,
        body.recycle_retention_unit,
    )
    .await
    {
        Ok(s) => Json(s).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct RecycleListQuery {
    bucket: Option<String>,
}

/// Same visibility rules as GET /recycle-bin: optional bucket, else owner=all / non-owner=readable.
async fn recycle_bin_bucket_filter(
    state: &AppState,
    auth: &AuthAccount,
    bucket: Option<&str>,
) -> Result<Option<Vec<i64>>, Response> {
    let is_owner = match rbac::account_is_owner(&state.db, auth.id()).await {
        Ok(v) => v,
        Err(err) => return Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()),
    };

    if let Some(name) = bucket {
        let (bid, _) = resolve_bucket_id(state, Some(name)).await?;
        require_bucket_perm(state, auth.id(), bid, CrudAction::Read).await?;
        Ok(Some(vec![bid]))
    } else if is_owner {
        Ok(None)
    } else {
        let buckets = match rbac::list_buckets(&state.db).await {
            Ok(b) => b,
            Err(err) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response());
            }
        };
        let mut readable = Vec::new();
        for b in buckets {
            match rbac::check_perm(&state.db, auth.id(), b.id, CrudAction::Read).await {
                Ok(true) => readable.push(b.id),
                Ok(false) => {}
                Err(err) => {
                    return Err(
                        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
                    );
                }
            }
        }
        Ok(Some(readable))
    }
}

async fn list_recycle_bin(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<RecycleListQuery>,
) -> Response {
    let bucket_filter = match recycle_bin_bucket_filter(&state, &auth, q.bucket.as_deref()).await {
        Ok(f) => f,
        Err(r) => return r,
    };

    match repository::list_deleted_objects(&state.db, bucket_filter.as_deref()).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn restore_recycle_item(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    let obj = match repository::get_object_by_id(&state.db, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return (StatusCode::NOT_FOUND, "object not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if let Err(r) = require_bucket_perm(&state, auth.id(), obj.bucket_id, CrudAction::Update).await
    {
        return r;
    }
    match repository::restore_object(&state.db, id).await {
        Ok(o) => Json(o).into_response(),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("not in recycle") || msg.contains("already exists") {
                (StatusCode::CONFLICT, msg).into_response()
            } else {
                (StatusCode::BAD_REQUEST, msg).into_response()
            }
        }
    }
}

async fn hard_delete_recycle_item(
    State(state): State<AppState>,
    auth: AuthAccount,
    Path(id): Path<i64>,
) -> Response {
    let obj = match repository::get_object_by_id(&state.db, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return (StatusCode::NOT_FOUND, "object not found").into_response(),
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };
    if obj.deleted_at.is_none() {
        return (StatusCode::BAD_REQUEST, "object is not in recycle bin").into_response();
    }
    if let Err(r) = require_bucket_perm(&state, auth.id(), obj.bucket_id, CrudAction::Delete).await
    {
        return r;
    }
    match hard_purge_object(&state, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct PurgeBody {
    ids: Vec<i64>,
}

#[derive(Serialize)]
struct PurgeResponse {
    deleted: u64,
    failed: Vec<i64>,
}

async fn purge_recycle_items(
    State(state): State<AppState>,
    auth: AuthAccount,
    Json(body): Json<PurgeBody>,
) -> Response {
    if body.ids.is_empty() {
        return (StatusCode::BAD_REQUEST, "ids must not be empty").into_response();
    }
    if body.ids.len() > 1000 {
        return (StatusCode::BAD_REQUEST, "too many ids (max 1000)").into_response();
    }

    let mut deleted: u64 = 0;
    let mut failed: Vec<i64> = Vec::new();

    for id in body.ids {
        let obj = match repository::get_object_by_id(&state.db, id).await {
            Ok(Some(o)) => o,
            Ok(None) => {
                failed.push(id);
                continue;
            }
            Err(_) => {
                failed.push(id);
                continue;
            }
        };
        if obj.deleted_at.is_none() {
            failed.push(id);
            continue;
        }
        if require_bucket_perm(&state, auth.id(), obj.bucket_id, CrudAction::Delete)
            .await
            .is_err()
        {
            failed.push(id);
            continue;
        }
        match hard_purge_object(&state, id).await {
            Ok(()) => deleted += 1,
            Err(_) => failed.push(id),
        }
    }

    Json(PurgeResponse { deleted, failed }).into_response()
}

async fn purge_all_recycle_items(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<RecycleListQuery>,
) -> Response {
    let bucket_filter = match recycle_bin_bucket_filter(&state, &auth, q.bucket.as_deref()).await {
        Ok(f) => f,
        Err(r) => return r,
    };

    let rows = match repository::list_deleted_objects(&state.db, bucket_filter.as_deref()).await {
        Ok(rows) => rows,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let mut deleted: u64 = 0;
    let mut failed: Vec<i64> = Vec::new();

    for obj in rows {
        if require_bucket_perm(&state, auth.id(), obj.bucket_id, CrudAction::Delete)
            .await
            .is_err()
        {
            failed.push(obj.id);
            continue;
        }
        match hard_purge_object(&state, obj.id).await {
            Ok(()) => deleted += 1,
            Err(_) => failed.push(obj.id),
        }
    }

    Json(PurgeResponse { deleted, failed }).into_response()
}

pub async fn hard_purge_object(state: &AppState, id: i64) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let record = sqlx::query_as::<_, crate::db::models::ObjectRecord>(
        r#"SELECT * FROM object WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(record) = record else {
        tx.rollback().await?;
        anyhow::bail!("object not found");
    };
    outbox::enqueue_delete_tx(
        &mut tx,
        record.id,
        &record.filepath,
        &record.etag,
        &state.config.node_id,
    )
    .await?;
    sqlx::query(r#"DELETE FROM object WHERE id = ?1"#)
        .bind(record.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let _ = state.engine.unlink(&record.filepath).await;
    Ok(())
}
