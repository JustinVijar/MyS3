use std::collections::HashMap;
use std::pin::Pin;
use std::str::FromStr;

use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::cluster::proto::object_replication_service_server::ObjectReplicationService;
use crate::cluster::proto::{
    object_chunk, DeleteRequest, DigestRequest, DigestResponse, NodeHeartbeat, NodeHeartbeatAck,
    ObjectChunk, ObjectMetadata, ReplicationAck,
};
use crate::db::models::EtagType;
use crate::db::{rbac, repository};
use crate::storage::StorageEngine;
use crate::tui::events::ServerEvent;

pub struct NodeReplicationService {
    pub db: SqlitePool,
    pub engine: StorageEngine,
    pub node_id: String,
    pub events: mpsc::UnboundedSender<ServerEvent>,
}

#[tonic::async_trait]
impl ObjectReplicationService for NodeReplicationService {
    async fn replicate_object(
        &self,
        request: Request<Streaming<ObjectChunk>>,
    ) -> Result<Response<ReplicationAck>, Status> {
        let mut stream = request.into_inner();
        let mut metadata: Option<ObjectMetadata> = None;
        let mut chunks: Vec<Result<bytes::Bytes, anyhow::Error>> = Vec::new();

        while let Some(chunk_res) = stream.message().await? {
            match chunk_res.content {
                Some(object_chunk::Content::Metadata(meta)) => {
                    metadata = Some(meta);
                }
                Some(object_chunk::Content::Payload(bytes)) => {
                    chunks.push(Ok(bytes::Bytes::from(bytes)));
                }
                None => {}
            }
        }

        let meta = metadata.ok_or_else(|| Status::invalid_argument("Missing object metadata"))?;
        let etag_type = EtagType::from_str(&meta.etag_type).unwrap_or(EtagType::Md5);

        let chunk_stream = futures::stream::iter(chunks);
        let stored = self
            .engine
            .put_chunks(
                chunk_stream,
                &meta.original_filename,
                etag_type,
                Some(&meta.etag),
                Some(&meta.filepath_uuid),
            )
            .await
            .map_err(|e| Status::internal(format!("write/verify failed: {e:#}")))?;

        let uploaded = (!meta.date_uploaded.is_empty()).then_some(meta.date_uploaded.as_str());
        let modified = (!meta.date_modified.is_empty()).then_some(meta.date_modified.as_str());
        let bucket_name = if meta.bucket_name.trim().is_empty() {
            "storage"
        } else {
            meta.bucket_name.trim()
        };
        let bucket_id = rbac::ensure_bucket(&self.db, bucket_name)
            .await
            .map_err(|e| Status::internal(format!("bucket resolve: {e:#}")))?;
        repository::insert_object_idempotent(
            &self.db,
            &meta.original_filename,
            &stored.filepath,
            &meta.file_format,
            stored.filesize_bytes,
            &meta.etag_type,
            &stored.etag,
            uploaded,
            modified,
            bucket_id,
        )
        .await
        .map_err(|e| Status::internal(format!("SQLite error: {e:#}")))?;

        let _ = self.events.send(ServerEvent::ObjectCreated {
            filename: meta.original_filename.clone(),
            size: stored.filesize_bytes,
        });
        info!(
            "replicated object {} etag={}",
            meta.original_filename, stored.etag
        );

        Ok(Response::new(ReplicationAck {
            success: true,
            message: "Object successfully replicated".to_string(),
            etag_confirmed: stored.etag,
        }))
    }

    async fn delete_object(
        &self,
        request: Request<DeleteRequest>,
    ) -> Result<Response<ReplicationAck>, Status> {
        let req = request.into_inner();
        let _ = self.engine.unlink(&req.filepath_uuid).await;
        let _ = repository::delete_object_by_filepath(&self.db, &req.filepath_uuid)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(ReplicationAck {
            success: true,
            message: "Object deleted (idempotent)".to_string(),
            etag_confirmed: req.etag,
        }))
    }

    async fn ping_node(
        &self,
        request: Request<NodeHeartbeat>,
    ) -> Result<Response<NodeHeartbeatAck>, Status> {
        let hb = request.into_inner();
        // Upsert peer on heartbeat so topology discovers itself.
        if let Err(err) = repository::upsert_peer(
            &self.db,
            &hb.node_id,
            &format!("{}:50051", hb.wireguard_ip),
        )
        .await
        {
            warn!("ping upsert peer: {err:#}");
        }
        if let Err(err) = crate::cluster::peer_manager::note_heartbeat(&self.db, &hb.node_id).await
        {
            warn!("ping heartbeat: {err:#}");
        }
        let _ = self
            .events
            .send(ServerEvent::PeerConnected(hb.node_id.clone()));

        Ok(Response::new(NodeHeartbeatAck {
            node_id: self.node_id.clone(),
            healthy: true,
        }))
    }

    async fn sync_digest(
        &self,
        request: Request<DigestRequest>,
    ) -> Result<Response<DigestResponse>, Status> {
        let prefix = request.into_inner().bucket_or_prefix;
        let map = repository::digest_map(&self.db, &prefix)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(DigestResponse {
            object_etags: map,
        }))
    }
}

pub async fn serve(
    addr: std::net::SocketAddr,
    service: NodeReplicationService,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), tonic::transport::Error> {
    use crate::cluster::proto::object_replication_service_server::ObjectReplicationServiceServer;
    use tonic::transport::Server;

    info!("gRPC replication listening on {addr}");
    Server::builder()
        .add_service(ObjectReplicationServiceServer::new(service))
        .serve_with_shutdown(addr, async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                if shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
}

#[allow(dead_code)]
fn _pin_marker(_: Pin<&mut HashMap<String, String>>) {}
