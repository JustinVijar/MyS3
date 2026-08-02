#[derive(Debug, Clone)]
pub enum ServerEvent {
    BytesUploaded(usize),
    BytesDownloaded(usize),
    ObjectCreated { filename: String, size: i64 },
    PeerConnected(String),
}
