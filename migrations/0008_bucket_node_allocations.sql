-- Per-bucket node allocations: capacity + soft/hard quota mode.

ALTER TABLE bucket_replication_peer
    ADD COLUMN allocated_bytes INTEGER NOT NULL DEFAULT 107374182400;

ALTER TABLE bucket_replication_peer
    ADD COLUMN quota_mode TEXT NOT NULL DEFAULT 'soft'
        CHECK (quota_mode IN ('soft', 'hard'));
