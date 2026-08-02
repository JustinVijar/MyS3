CREATE TABLE IF NOT EXISTS object (
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
    date_modified DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_object_etag ON object(etag);
CREATE INDEX IF NOT EXISTS idx_object_filename ON object(original_filename);
