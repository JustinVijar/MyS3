-- Track WireGuard peer nodes in the cluster
CREATE TABLE IF NOT EXISTS cluster_peer (
    id TEXT PRIMARY KEY,               -- e.g., 'node-philippines-1'
    wireguard_endpoint TEXT NOT NULL,  -- e.g., '10.0.0.2:50051'
    is_active BOOLEAN DEFAULT TRUE,
    last_heartbeat_utc DATETIME
);

-- Transactional outbox queue for async replication
-- object_id uses ON DELETE SET NULL + denormalized filepath_uuid/etag so DELETE
-- jobs survive local object row removal.
CREATE TABLE IF NOT EXISTS replication_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    peer_id TEXT NOT NULL,
    object_id INTEGER,
    filepath_uuid TEXT,
    etag TEXT,
    operation TEXT NOT NULL CHECK(operation IN ('PUT', 'DELETE')),
    status TEXT NOT NULL DEFAULT 'PENDING' CHECK(status IN ('PENDING', 'IN_FLIGHT', 'COMPLETED', 'FAILED')),
    attempt_count INTEGER DEFAULT 0,
    next_retry_utc DATETIME DEFAULT CURRENT_TIMESTAMP,
    created_utc DATETIME DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(peer_id) REFERENCES cluster_peer(id),
    FOREIGN KEY(object_id) REFERENCES object(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_outbox_pending
ON replication_outbox(status, next_retry_utc)
WHERE status IN ('PENDING', 'FAILED');
