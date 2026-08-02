# MyS3

A self-hosted, S3-style object store written in Rust. MyS3 combines a SigV4-authenticated object API, a browser UI, an optional terminal dashboard, and multi-node replication over gRPC (optionally tunneled with WireGuard).

## Features

- **Object storage API** — put / get / delete / list with AWS SigV4 auth
- **Web UI** — browse buckets, upload/download, previews, folder archives, settings
- **Share links** — time-limited public links (token or short code)
- **RBAC** — accounts, roles, and per-bucket CRUD permissions
- **Recycle bin** — soft-delete with retention purge
- **Content hashing** — configurable ETags (`md5`, `sha256`, `sha512`, `blake2`, `blake3`)
- **Clustering** — gRPC replication, outbox worker, and anti-entropy sync
- **WireGuard** — optional embedded userspace tunnel for peer traffic
- **TUI** — local ratatui dashboard (disabled automatically when not on a TTY)

## Requirements

- [Rust](https://rustup.rs/) (edition 2024; Docker image builds with Rust 1.97+)
- Docker / Docker Compose (optional, for containerized runs)
- A modern browser for the Web UI (video preview uses ffmpeg.wasm + MediaSource in the client)

## Quick start

### Native

```bash
cargo build --release

# Bind on loopback for local use
DISABLE_TUI=1 ./target/release/rust-s3-engine serve \
  --bind 127.0.0.1:9000 \
  --grpc-bind 127.0.0.1:50051 \
  --storage ./.data
```

Open the UI at [http://127.0.0.1:9000](http://127.0.0.1:9000). On first visit, create the owner account (hex credentials are shown once).

Default S3 credentials (overridable via env):

| Variable | Default |
|---|---|
| `AWS_ACCESS_KEY_ID` | `minioadmin` |
| `AWS_SECRET_ACCESS_KEY` | `minioadmin` |

### Docker Compose

```bash
docker compose up -d --build
```

- HTTP: `http://localhost:9000` (override with `MYS3_HTTP_PORT`)
- gRPC: `localhost:50051` (override with `MYS3_GRPC_PORT`)
- Data: Docker volume `mys3-data`, or host dir via `MYS3_DATA=./.data`

## Configuration

CLI flags take precedence over environment variables.

| Source | Purpose |
|---|---|
| `--bind` / `WIREGUARD_BIND_ADDR` | HTTP listen address (default `10.0.0.1:9000`) |
| `--grpc-bind` / `GRPC_BIND_ADDR` | gRPC listen address (default `10.0.0.1:50051`) |
| `--storage` / `STORAGE_ROOT` | Data root (default `./.data`, or `.mys3/storage_root`) |
| `NODE_ID` | Cluster node identity (default `node-local-1`) |
| `CLUSTER_PEERS` | Peer seeds: `id=host:port,id2=host:port` |
| `DEFAULT_ETAG_TYPE` | Default hash algorithm (default `md5`) |
| `DISABLE_TUI` | `1` to run headless |
| `EMBED_WG` | `1` to enable embedded WireGuard |
| `WG_PRIVATE_KEY` | WireGuard private key when embedding WG |
| `RUST_LOG` | Tracing filter (default `info`) |

On disk under the storage root:

```
<data>/
  objects/       # object payloads
  metadata.db    # SQLite metadata + migrations
```

## Architecture

```
┌─────────────┐     SigV4 / session      ┌──────────────────┐
│  S3 clients │ ───────────────────────► │  Axum HTTP (:9000)│
│  Web UI     │                          │  + embedded UI    │
└─────────────┘                          └────────┬─────────┘
                                                  │
                                         SQLite + storage engine
                                                  │
┌─────────────┐     gRPC replication     ┌────────▼─────────┐
│ Peer nodes  │ ◄──────────────────────► │ gRPC (:50051)    │
└─────────────┘   (optional WireGuard)   │ outbox + AE sync │
                                         └──────────────────┘
```

Key crates / modules:

- `src/server` — S3 routes, web/settings/share APIs, SigV4 + session auth
- `src/storage` — content-addressed object engine, hashing, reconcile
- `src/cluster` — peer manager, gRPC client/server, outbox, anti-entropy
- `src/db` — SQLite repository, RBAC, share links
- `src/tui` — terminal dashboard
- `web-ui/` — static SPA served via `rust-embed`
- `proto/replication.proto` — cluster replication RPCs
- `migrations/` — schema evolution (sqlx)

## S3 API (subset)

Authenticated with SigV4 using `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`:

| Method | Path | Action |
|---|---|---|
| `GET` | `/storage/objects` | List objects |
| `PUT` | `/storage/objects/{key}` | Upload |
| `GET` | `/storage/objects/{key}` | Download |
| `DELETE` | `/storage/objects/{key}` | Delete |

The web UI and JSON APIs under `/api/v1/*` use session auth after bootstrap/login.

## Clustering

1. Start each node with a unique `NODE_ID` and reachable `GRPC_BIND_ADDR`.
2. Seed peers with `CLUSTER_PEERS=peer-id=host:port,...`.
3. Objects are pushed via the replication outbox; anti-entropy reconciles digests over time.
4. Optionally set `EMBED_WG=1` and `WG_PRIVATE_KEY` so peer traffic rides WireGuard.

## Recovery

Reset an account password against the metadata database:

```bash
cargo run --example reset_password -- ./.data/metadata.db <account_id>
```

A new hex password is printed once.

## Development

```bash
cargo build
cargo test
cargo run -- serve --bind 127.0.0.1:9000 --storage ./.data
```

Protobuf code is generated at build time (`build.rs` + `tonic-build`). Migrations apply automatically on startup.

## License

Proprietary / unlicensed unless otherwise stated by the repository owner.
