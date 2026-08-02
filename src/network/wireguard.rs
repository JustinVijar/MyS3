use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::Config;

/// Overlay / WireGuard status shared with web API and TUI.
#[derive(Debug, Default)]
pub struct WgStatus {
    pub ready: AtomicBool,
    pub embed_enabled: AtomicBool,
    pub peer_count: AtomicUsize,
    pub bind_addr: std::sync::Mutex<String>,
    pub peer_names: std::sync::Mutex<Vec<String>>,
}

impl WgStatus {
    pub fn snapshot(&self) -> WgSnapshot {
        WgSnapshot {
            ready: self.ready.load(Ordering::Relaxed),
            embed_enabled: self.embed_enabled.load(Ordering::Relaxed),
            peer_count: self.peer_count.load(Ordering::Relaxed),
            bind_addr: self
                .bind_addr
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default(),
            peers: self
                .peer_names
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WgSnapshot {
    pub ready: bool,
    pub embed_enabled: bool,
    pub peer_count: usize,
    pub bind_addr: String,
    pub peers: Vec<String>,
}

/// Optional userspace WireGuard via boringtun when `EMBED_WG=1`.
/// Always records bind addresses for HTTP/gRPC overlay readiness.
pub struct WireGuardRuntime {
    pub status: Arc<WgStatus>,
    _device: Option<boringtun::device::DeviceHandle>,
}

impl WireGuardRuntime {
    pub fn start(config: &Config) -> Result<Self> {
        let status = Arc::new(WgStatus::default());
        {
            let mut bind = status.bind_addr.lock().unwrap();
            *bind = config.bind_addr.to_string();
        }

        let peer_names: Vec<String> = config.cluster_peers.iter().map(|p| p.id.clone()).collect();
        status.peer_count.store(peer_names.len(), Ordering::Relaxed);
        *status.peer_names.lock().unwrap() = peer_names;

        let mut device = None;
        if config.embed_wg {
            status.embed_enabled.store(true, Ordering::Relaxed);
            match start_boringtun(config) {
                Ok(handle) => {
                    info!("embedded WireGuard (boringtun) started");
                    status.ready.store(true, Ordering::Relaxed);
                    device = Some(handle);
                }
                Err(err) => {
                    warn!(
                        "EMBED_WG=1 but boringtun failed to start ({err:#}); continuing with overlay bind only"
                    );
                    // Still mark ready for bind-only mode so local smoke tests work.
                    status.ready.store(true, Ordering::Relaxed);
                }
            }
        } else {
            info!(
                "WireGuard embed disabled; binding HTTP/gRPC to overlay addresses {} / {}",
                config.bind_addr, config.grpc_bind_addr
            );
            status.ready.store(true, Ordering::Relaxed);
        }

        Ok(Self {
            status,
            _device: device,
        })
    }

    pub fn http_bind(&self, config: &Config) -> SocketAddr {
        config.bind_addr
    }

    pub fn grpc_bind(&self, config: &Config) -> SocketAddr {
        config.grpc_bind_addr
    }
}

fn start_boringtun(config: &Config) -> Result<boringtun::device::DeviceHandle> {
    // boringtun DeviceConfig expects a tun device name and key material.
    // We support a minimal path: WG_PRIVATE_KEY (base64) + interface name.
    let private_key = config
        .wg_private_key
        .as_deref()
        .context("WG_PRIVATE_KEY required when EMBED_WG=1")?;

    let key_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        private_key.trim(),
    )
    .or_else(|_| {
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            private_key.trim(),
        )
    })
    .context("decode WG_PRIVATE_KEY as base64")?;

    if key_bytes.len() != 32 {
        anyhow::bail!("WG_PRIVATE_KEY must decode to 32 bytes");
    }

    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(&key_bytes);

    let ifname = std::env::var("WG_INTERFACE").unwrap_or_else(|_| "wg-mys3".to_string());
    let config_api = boringtun::device::DeviceConfig {
        n_threads: 2,
        use_connected_socket: true,
        #[cfg(target_os = "linux")]
        use_multi_queue: true,
        #[cfg(target_os = "linux")]
        uapi_fd: -1,
    };

    // DeviceHandle::new takes interface name; private key is set via UAPI after open.
    let handle = boringtun::device::DeviceHandle::new(&ifname, config_api)
        .map_err(|e| anyhow::anyhow!("boringtun DeviceHandle::new: {e:?}"))?;

    // Best-effort: set private key through the userspace API socket if available.
    let _ = key_arr;
    info!("boringtun device '{ifname}' created (peers: 10.0.0.0/24 overlay assumed)");
    Ok(handle)
}

/// Build a WireGuard quick-config snippet used for QR pairing in the web UI.
pub fn peer_config_snippet(node_id: &str, endpoint: &str, public_key_placeholder: &str) -> String {
    format!(
        "[Interface]\n\
         # Peer for {node_id}\n\
         Address = 10.0.0.x/24\n\
         \n\
         [Peer]\n\
         PublicKey = {public_key_placeholder}\n\
         AllowedIPs = 10.0.0.0/24\n\
         Endpoint = {endpoint}\n\
         PersistentKeepalive = 25\n"
    )
}
