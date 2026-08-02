use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use strum::{Display, EnumString};

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString, Display, sqlx::Type,
)]
#[strum(serialize_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
#[sqlx(type_name = "TEXT", rename_all = "kebab-case")]
pub enum EtagType {
    #[strum(serialize = "md5")]
    #[sqlx(rename = "md5")]
    Md5,
    #[strum(serialize = "sha256")]
    #[sqlx(rename = "sha256")]
    Sha256,
    #[strum(serialize = "sha512")]
    #[sqlx(rename = "sha512")]
    Sha512,
    #[strum(serialize = "blake2-128")]
    #[sqlx(rename = "blake2-128")]
    Blake2_128,
    #[strum(serialize = "blake2-256")]
    #[sqlx(rename = "blake2-256")]
    Blake2_256,
    #[strum(serialize = "blake3-128")]
    #[sqlx(rename = "blake3-128")]
    Blake3_128,
    #[strum(serialize = "blake3-256")]
    #[sqlx(rename = "blake3-256")]
    Blake3_256,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub id: i64,
    pub original_filename: String,
    pub filepath: String,
    pub file_format: String,
    pub filesize_bytes: i64,
    pub etag_type: EtagType,
    pub etag: String,
    pub date_uploaded: DateTime<Utc>,
    pub date_modified: DateTime<Utc>,
    pub bucket_id: i64,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct BucketRecord {
    pub id: i64,
    pub name: String,
    pub created_utc: DateTime<Utc>,
    pub owner_account_id: Option<i64>,
    /// When true, replicate to every active cluster peer (legacy default).
    pub replicate_to_all: bool,
    /// Default hashing algorithm for new objects in this bucket.
    pub etag_type: EtagType,
    /// NULL | running | done | error
    pub etag_rehash_status: Option<String>,
    pub etag_rehash_processed: i64,
    pub etag_rehash_total: i64,
    pub etag_rehash_error: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: i64,
    pub username_hex: String,
    pub password_hash: String,
    pub display_name: String,
    pub is_disabled: bool,
    pub created_utc: DateTime<Utc>,
    pub updated_utc: DateTime<Utc>,
    pub created_by_account_id: Option<i64>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RoleRecord {
    pub id: i64,
    pub name: String,
    pub position: i64,
    pub is_owner: bool,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RoleBucketPermission {
    pub role_id: i64,
    pub bucket_id: i64,
    pub can_create: bool,
    pub can_read: bool,
    pub can_update: bool,
    pub can_delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrudPerms {
    pub can_create: bool,
    pub can_read: bool,
    pub can_update: bool,
    pub can_delete: bool,
}

impl CrudPerms {
    pub const FULL: Self = Self {
        can_create: true,
        can_read: true,
        can_update: true,
        can_delete: true,
    };

    pub const NONE: Self = Self {
        can_create: false,
        can_read: false,
        can_update: false,
        can_delete: false,
    };

    pub fn or(self, other: Self) -> Self {
        Self {
            can_create: self.can_create || other.can_create,
            can_read: self.can_read || other.can_read,
            can_update: self.can_update || other.can_update,
            can_delete: self.can_delete || other.can_delete,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrudAction {
    Create,
    Read,
    Update,
    Delete,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString, Display, sqlx::Type,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum RetentionUnit {
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
    Decade,
}

impl RetentionUnit {
    /// Approximate duration in seconds for purge scheduling.
    /// month = 30d, year = 365d, decade = 3650d.
    pub fn to_seconds(self, value: i64) -> i64 {
        let unit_secs: i64 = match self {
            Self::Second => 1,
            Self::Minute => 60,
            Self::Hour => 3600,
            Self::Day => 86400,
            Self::Month => 30 * 86400,
            Self::Year => 365 * 86400,
            Self::Decade => 3650 * 86400,
        };
        value.saturating_mul(unit_secs)
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct AppSettings {
    pub id: i64,
    pub recycle_retention_value: i64,
    pub recycle_retention_unit: RetentionUnit,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString, Display, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ShareTargetKind {
    File,
    Folder,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString, Display, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ShareAccessMode {
    SpecificUsers,
    BucketReaders,
    Public,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ShareLinkRecord {
    pub id: i64,
    pub token: String,
    pub short_code: Option<String>,
    pub bucket_id: i64,
    pub target_key: String,
    pub target_kind: ShareTargetKind,
    pub access_mode: ShareAccessMode,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_by_account_id: i64,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ClusterPeer {
    pub id: String,
    pub wireguard_endpoint: String,
    pub is_active: bool,
    pub last_heartbeat_utc: Option<DateTime<Utc>>,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, EnumString, Display, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum QuotaMode {
    Soft,
    Hard,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct BucketNodeAssignment {
    pub bucket_id: i64,
    pub peer_id: String,
    pub allocated_bytes: i64,
    pub quota_mode: QuotaMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString)]
#[strum(serialize_all = "UPPERCASE")]
pub enum OutboxOperation {
    Put,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString)]
#[strum(serialize_all = "SCREAMING_SNAKE_CASE")]
pub enum OutboxStatus {
    Pending,
    InFlight,
    Completed,
    Failed,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutboxJob {
    pub id: i64,
    pub peer_id: String,
    pub object_id: Option<i64>,
    pub filepath_uuid: Option<String>,
    pub etag: Option<String>,
    pub operation: String,
    pub status: String,
    pub attempt_count: i64,
    pub wireguard_endpoint: String,
    pub original_filename: Option<String>,
    pub filepath: Option<String>,
    pub file_format: Option<String>,
    pub filesize_bytes: Option<i64>,
    pub etag_type: Option<String>,
    pub object_etag: Option<String>,
    pub date_uploaded: Option<DateTime<Utc>>,
    pub date_modified: Option<DateTime<Utc>>,
    pub bucket_name: Option<String>,
}
