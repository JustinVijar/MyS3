-- Per-bucket default ETag algorithm and rehash progress.

ALTER TABLE bucket
    ADD COLUMN etag_type TEXT NOT NULL DEFAULT 'md5'
        CHECK (etag_type IN (
            'md5',
            'sha256',
            'sha512',
            'blake2-128',
            'blake2-256',
            'blake3-128',
            'blake3-256'
        ));

ALTER TABLE bucket ADD COLUMN etag_rehash_status TEXT;
ALTER TABLE bucket ADD COLUMN etag_rehash_processed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE bucket ADD COLUMN etag_rehash_total INTEGER NOT NULL DEFAULT 0;
ALTER TABLE bucket ADD COLUMN etag_rehash_error TEXT;
