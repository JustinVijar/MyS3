use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};

use crate::db::models::EtagType;

/// CWD-relative file that persists the UI-chosen storage root across restarts.
pub const STORAGE_ROOT_OVERRIDE_FILE: &str = ".mys3/storage_root";

#[derive(Debug, Clone)]
pub struct PeerSeed {
    pub id: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Default)]
struct CliOverrides {
    bind_addr: Option<String>,
    grpc_bind_addr: Option<String>,
    storage_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub storage_root: PathBuf,
    pub bind_addr: SocketAddr,
    pub grpc_bind_addr: SocketAddr,
    pub embed_wg: bool,
    pub node_id: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub default_etag_type: EtagType,
    pub cluster_peers: Vec<PeerSeed>,
    pub wg_private_key: Option<String>,
    pub disable_tui: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let cli = parse_cli_overrides(env::args().skip(1));

        // Precedence: CLI --storage > STORAGE_ROOT env > .mys3/storage_root > ./.data
        let storage_root = cli
            .storage_root
            .clone()
            .or_else(|| env::var("STORAGE_ROOT").ok().map(PathBuf::from))
            .or_else(read_persisted_storage_root)
            .unwrap_or_else(|| PathBuf::from(".data"));

        let bind_addr = cli
            .bind_addr
            .clone()
            .or_else(|| env::var("WIREGUARD_BIND_ADDR").ok())
            .unwrap_or_else(|| "10.0.0.1:9000".to_string())
            .parse::<SocketAddr>()?;

        let grpc_bind_addr = cli
            .grpc_bind_addr
            .clone()
            .or_else(|| env::var("GRPC_BIND_ADDR").ok())
            .unwrap_or_else(|| {
                // When binding HTTP locally via CLI/env loopback, keep gRPC on the same host.
                if bind_addr.ip().is_loopback() {
                    format!("{}:50051", bind_addr.ip())
                } else {
                    "10.0.0.1:50051".to_string()
                }
            })
            .parse::<SocketAddr>()?;

        let embed_wg = matches!(
            env::var("EMBED_WG").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
        );

        let node_id = env::var("NODE_ID").unwrap_or_else(|_| "node-local-1".to_string());

        let aws_access_key_id =
            env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "minioadmin".to_string());
        let aws_secret_access_key =
            env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());

        let default_etag_type = env::var("DEFAULT_ETAG_TYPE")
            .ok()
            .and_then(|s| EtagType::from_str(&s).ok())
            .unwrap_or(EtagType::Md5);

        let cluster_peers = parse_cluster_peers(
            &env::var("CLUSTER_PEERS").unwrap_or_default(),
        );

        let wg_private_key = env::var("WG_PRIVATE_KEY").ok();
        let disable_tui = matches!(
            env::var("DISABLE_TUI").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
        ) || !std::io::IsTerminal::is_terminal(&std::io::stdout());

        Ok(Self {
            storage_root,
            bind_addr,
            grpc_bind_addr,
            embed_wg,
            node_id,
            aws_access_key_id,
            aws_secret_access_key,
            default_etag_type,
            cluster_peers,
            wg_private_key,
            disable_tui,
        })
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.storage_root.join("objects")
    }

    pub fn metadata_db_path(&self) -> PathBuf {
        self.storage_root.join("metadata.db")
    }
}

fn read_persisted_storage_root() -> Option<PathBuf> {
    let raw = fs::read_to_string(STORAGE_ROOT_OVERRIDE_FILE).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Resolve a user-supplied storage path to an absolute, normalized path.
pub fn resolve_storage_path(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("storage path is required");
    }
    let path = PathBuf::from(trimmed);
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .context("resolve current directory")?
            .join(path)
    };
    // Normalize `.` / `..` without requiring the path to exist yet.
    Ok(normalize_path(&absolute))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Persist absolute storage root for the next process start (CWD-relative `.mys3/storage_root`).
pub fn persist_storage_root(path: &Path) -> Result<()> {
    let absolute = if path.is_absolute() {
        normalize_path(path)
    } else {
        resolve_storage_path(&path.to_string_lossy())?
    };
    let parent = Path::new(".mys3");
    fs::create_dir_all(parent).context("create .mys3 directory")?;
    fs::write(
        STORAGE_ROOT_OVERRIDE_FILE,
        format!("{}\n", absolute.display()),
    )
    .context("write storage root override")?;
    Ok(())
}

/// Parse `serve [--bind ADDR] [--storage PATH] [--grpc-bind ADDR]`.
fn parse_cli_overrides<I, S>(args: I) -> CliOverrides
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = CliOverrides::default();
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut i = 0usize;
    // Allow both `serve --bind ...` and bare `--bind ...`.
    while i < args.len() {
        match args[i].as_str() {
            "serve" => {
                i += 1;
            }
            "--bind" | "-b" => {
                if let Some(v) = args.get(i + 1) {
                    out.bind_addr = Some(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--storage" | "--storage-root" => {
                if let Some(v) = args.get(i + 1) {
                    out.storage_root = Some(PathBuf::from(v));
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--grpc-bind" => {
                if let Some(v) = args.get(i + 1) {
                    out.grpc_bind_addr = Some(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            other if other.starts_with("--bind=") => {
                out.bind_addr = Some(other["--bind=".len()..].to_string());
                i += 1;
            }
            other if other.starts_with("--storage=") => {
                out.storage_root = Some(PathBuf::from(&other["--storage=".len()..]));
                i += 1;
            }
            other if other.starts_with("--storage-root=") => {
                out.storage_root = Some(PathBuf::from(&other["--storage-root=".len()..]));
                i += 1;
            }
            other if other.starts_with("--grpc-bind=") => {
                out.grpc_bind_addr = Some(other["--grpc-bind=".len()..].to_string());
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    out
}

fn parse_cluster_peers(raw: &str) -> Vec<PeerSeed> {
    raw.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (id, endpoint) = entry.split_once('=')?;
            Some(PeerSeed {
                id: id.trim().to_string(),
                endpoint: endpoint.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serve_bind_and_storage() {
        let cli = parse_cli_overrides([
            "serve",
            "--bind",
            "127.0.0.1:18080",
            "--storage",
            "./.data",
        ]);
        assert_eq!(cli.bind_addr.as_deref(), Some("127.0.0.1:18080"));
        assert_eq!(
            cli.storage_root.as_deref(),
            Some(std::path::Path::new("./.data"))
        );
    }
}
