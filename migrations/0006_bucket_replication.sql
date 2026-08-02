-- Per-bucket replication targets. Default replicate_to_all=1 keeps prior "all peers" behavior.

ALTER TABLE bucket ADD COLUMN replicate_to_all INTEGER NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS bucket_replication_peer (
    bucket_id INTEGER NOT NULL,
    peer_id TEXT NOT NULL,
    PRIMARY KEY (bucket_id, peer_id),
    FOREIGN KEY (bucket_id) REFERENCES bucket(id) ON DELETE CASCADE,
    FOREIGN KEY (peer_id) REFERENCES cluster_peer(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_bucket_replication_peer_peer
    ON bucket_replication_peer(peer_id);
