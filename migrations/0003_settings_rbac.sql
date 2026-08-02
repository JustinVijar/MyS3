-- Buckets, Discord-style RBAC, sessions, recycle-bin settings.
-- Soft-delete via object.deleted_at; active objects live in a bucket.

CREATE TABLE IF NOT EXISTS bucket (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    created_utc DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS account (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username_hex TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    is_disabled INTEGER NOT NULL DEFAULT 0,
    created_utc DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_utc DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS role (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    position INTEGER NOT NULL DEFAULT 0,
    is_owner INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS account_role (
    account_id INTEGER NOT NULL,
    role_id INTEGER NOT NULL,
    PRIMARY KEY (account_id, role_id),
    FOREIGN KEY (account_id) REFERENCES account(id) ON DELETE CASCADE,
    FOREIGN KEY (role_id) REFERENCES role(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS role_bucket_permission (
    role_id INTEGER NOT NULL,
    bucket_id INTEGER NOT NULL,
    can_create INTEGER NOT NULL DEFAULT 0,
    can_read INTEGER NOT NULL DEFAULT 0,
    can_update INTEGER NOT NULL DEFAULT 0,
    can_delete INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (role_id, bucket_id),
    FOREIGN KEY (role_id) REFERENCES role(id) ON DELETE CASCADE,
    FOREIGN KEY (bucket_id) REFERENCES bucket(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session (
    token TEXT PRIMARY KEY,
    account_id INTEGER NOT NULL,
    expires_utc DATETIME NOT NULL,
    created_utc DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (account_id) REFERENCES account(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_account ON session(account_id);
CREATE INDEX IF NOT EXISTS idx_session_expires ON session(expires_utc);

CREATE TABLE IF NOT EXISTS app_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    recycle_retention_value INTEGER NOT NULL DEFAULT 30,
    recycle_retention_unit TEXT NOT NULL DEFAULT 'day'
        CHECK (recycle_retention_unit IN (
            'second', 'minute', 'hour', 'day', 'month', 'year', 'decade'
        ))
);

-- Seed default bucket before altering object.
INSERT INTO bucket (name) VALUES ('storage');

-- Recreate object with bucket_id + deleted_at (SQLite cannot ADD NOT NULL FK cleanly).
PRAGMA foreign_keys = OFF;

CREATE TABLE object_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    original_filename TEXT NOT NULL,
    filepath TEXT NOT NULL UNIQUE,
    file_format TEXT NOT NULL,
    filesize_bytes INTEGER NOT NULL,
    etag_type TEXT NOT NULL CHECK(etag_type IN (
        'md5',
        'sha256',
        'sha512',
        'blake2-128',
        'blake2-256',
        'blake3-128',
        'blake3-256'
    )),
    etag TEXT NOT NULL,
    date_uploaded DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    date_modified DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    bucket_id INTEGER NOT NULL REFERENCES bucket(id),
    deleted_at DATETIME
);

INSERT INTO object_new (
    id, original_filename, filepath, file_format, filesize_bytes,
    etag_type, etag, date_uploaded, date_modified, bucket_id, deleted_at
)
SELECT
    o.id,
    o.original_filename,
    o.filepath,
    o.file_format,
    o.filesize_bytes,
    o.etag_type,
    o.etag,
    o.date_uploaded,
    o.date_modified,
    (SELECT id FROM bucket WHERE name = 'storage' LIMIT 1),
    NULL
FROM object o;

DROP TABLE object;
ALTER TABLE object_new RENAME TO object;

CREATE INDEX IF NOT EXISTS idx_object_etag ON object(etag);
CREATE INDEX IF NOT EXISTS idx_object_filename ON object(original_filename);
CREATE INDEX IF NOT EXISTS idx_object_bucket ON object(bucket_id);
CREATE INDEX IF NOT EXISTS idx_object_deleted_at ON object(deleted_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_object_bucket_key_active
    ON object(bucket_id, original_filename)
    WHERE deleted_at IS NULL;

PRAGMA foreign_keys = ON;

-- Seed Owner role with full CRUD on storage.
INSERT INTO role (name, position, is_owner) VALUES ('Owner', 1000, 1);

INSERT INTO role_bucket_permission (
    role_id, bucket_id, can_create, can_read, can_update, can_delete
)
SELECT r.id, b.id, 1, 1, 1, 1
FROM role r, bucket b
WHERE r.name = 'Owner' AND b.name = 'storage';

INSERT INTO app_settings (id, recycle_retention_value, recycle_retention_unit)
VALUES (1, 30, 'day');
