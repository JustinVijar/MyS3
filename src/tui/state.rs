use std::collections::VecDeque;

use crate::tui::events::ServerEvent;

#[derive(Debug, Clone)]
pub struct RecentObject {
    pub filename: String,
    pub size: i64,
    pub etag: String,
    pub algorithm: String,
}

#[derive(Debug)]
pub struct TuiState {
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub throughput_samples: VecDeque<u64>,
    pub peer_samples: VecDeque<u64>,
    pub peers: Vec<String>,
    pub recent: VecDeque<RecentObject>,
    pub disk_used: u64,
    pub disk_capacity: u64,
    pub object_count: u64,
    last_up: u64,
    last_down: u64,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            bytes_up: 0,
            bytes_down: 0,
            throughput_samples: VecDeque::from(vec![0; 60]),
            peer_samples: VecDeque::from(vec![0; 60]),
            peers: Vec::new(),
            recent: VecDeque::new(),
            disk_used: 0,
            disk_capacity: 1,
            object_count: 0,
            last_up: 0,
            last_down: 0,
        }
    }
}

impl TuiState {
    pub fn apply(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::BytesUploaded(n) => self.bytes_up += n as u64,
            ServerEvent::BytesDownloaded(n) => self.bytes_down += n as u64,
            ServerEvent::ObjectCreated { filename, size } => {
                self.object_count += 1;
                self.recent.push_front(RecentObject {
                    filename,
                    size,
                    etag: String::new(),
                    algorithm: String::new(),
                });
                while self.recent.len() > 50 {
                    self.recent.pop_back();
                }
            }
            ServerEvent::PeerConnected(name) => {
                if !self.peers.contains(&name) {
                    self.peers.push(name);
                }
            }
        }
    }

    pub fn tick_throughput(&mut self) {
        let delta = (self.bytes_up + self.bytes_down).saturating_sub(self.last_up + self.last_down);
        self.last_up = self.bytes_up;
        self.last_down = self.bytes_down;
        self.throughput_samples.push_back(delta);
        if self.throughput_samples.len() > 60 {
            self.throughput_samples.pop_front();
        }
        self.peer_samples.push_back(self.peers.len() as u64);
        if self.peer_samples.len() > 60 {
            self.peer_samples.pop_front();
        }
    }
}
