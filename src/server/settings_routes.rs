use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::cluster::outbox;
use crate::db::models::{CrudAction, CrudPerms, RetentionUnit};
use crate::db::rbac;
use crate::db::repository;
use crate::server::session_auth::{
    require_bucket_perm, require_owner, resolve_bucket_id, AuthAccount, SESSION_COOKIE,
};
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
            "/api/v1/settings/recycle",
            get(get_recycle_settings).put(put_recycle_settings),
        )
        .route("/api/v1/recycle-bin", get(list_recycle_bin))
        .route("/api/v1/recycle-bin/purge", post(purge_recycle_items))
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
    match rbac::create_bucket(&state.db, name, auth.id()).await {
        Ok(b) => (StatusCode::CREATED, Json(b)).into_response(),
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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

async fn list_recycle_bin(
    State(state): State<AppState>,
    auth: AuthAccount,
    Query(q): Query<RecycleListQuery>,
) -> Response {
    let is_owner = match rbac::account_is_owner(&state.db, auth.id()).await {
        Ok(v) => v,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    let bucket_filter = if let Some(name) = q.bucket.as_deref() {
        let (bid, _) = match resolve_bucket_id(&state, Some(name)).await {
            Ok(v) => v,
            Err(r) => return r,
        };
        if let Err(r) = require_bucket_perm(&state, auth.id(), bid, CrudAction::Read).await {
            return r;
        }
        Some(vec![bid])
    } else if is_owner {
        None
    } else {
        let buckets = match rbac::list_buckets(&state.db).await {
            Ok(b) => b,
            Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        };
        let mut readable = Vec::new();
        for b in buckets {
            match rbac::check_perm(&state.db, auth.id(), b.id, CrudAction::Read).await {
                Ok(true) => readable.push(b.id),
                Ok(false) => {}
                Err(err) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
                }
            }
        }
        Some(readable)
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
    outbox::enqueue_delete_tx(&mut tx, record.id, &record.filepath, &record.etag).await?;
    sqlx::query(r#"DELETE FROM object WHERE id = ?1"#)
        .bind(record.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let _ = state.engine.unlink(&record.filepath).await;
    Ok(())
}
