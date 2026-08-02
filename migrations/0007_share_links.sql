-- Share links for files and folders (web Explorer).

CREATE TABLE IF NOT EXISTS share_link (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token TEXT NOT NULL UNIQUE,
    short_code TEXT UNIQUE,
    bucket_id INTEGER NOT NULL REFERENCES bucket(id),
    target_key TEXT NOT NULL,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('file', 'folder')),
    access_mode TEXT NOT NULL CHECK (access_mode IN ('specific_users', 'bucket_readers', 'public')),
    expires_at DATETIME,
    created_by_account_id INTEGER NOT NULL REFERENCES account(id),
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    revoked_at DATETIME
);

CREATE TABLE IF NOT EXISTS share_link_user (
    share_id INTEGER NOT NULL REFERENCES share_link(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES account(id),
    PRIMARY KEY (share_id, account_id)
);

CREATE INDEX IF NOT EXISTS idx_share_link_target
    ON share_link (bucket_id, target_key)
    WHERE revoked_at IS NULL;
