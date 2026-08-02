use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tonic::transport::Channel;

use crate::cluster::proto::object_replication_service_client::ObjectReplicationServiceClient;
use crate::cluster::proto::{
    object_chunk, DeleteRequest, DigestRequest, DigestResponse, NodeHeartbeat, NodeHeartbeatAck,
    ObjectChunk, ObjectMetadata, ReplicationAck,
};
use crate::db::models::OutboxJob;
use crate::storage::StorageEngine;

pub type ReplicationClient = ObjectReplicationServiceClient<Channel>;

pub async fn connect(endpoint: &str) -> Result<ReplicationClient> {
    let uri = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    };
    let client = ObjectReplicationServiceClient::connect(uri.clone())
        .await
        .with_context(|| format!("connect gRPC {uri}"))?;
    Ok(client)
}

pub async fn replicate_put(
    client: &mut ReplicationClient,
    engine: &StorageEngine,
    job: &OutboxJob,
) -> Result<ReplicationAck> {
    let filepath = job
        .filepath
        .clone()
        .or_else(|| job.filepath_uuid.clone())
        .context("missing filepath for PUT job")?;
    let etag = job
        .object_etag
        .clone()
        .or_else(|| job.etag.clone())
        .unwrap_or_default();
    let original_filename = job
        .original_filename
        .clone()
        .unwrap_or_else(|| filepath.clone());
    let file_format = job.file_format.clone().unwrap_or_else(|| "bin".into());
    let filesize_bytes = job.filesize_bytes.unwrap_or(0);
    let etag_type = job.etag_type.clone().unwrap_or_else(|| "md5".into());
    let date_uploaded = job
        .date_uploaded
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();
    let date_modified = job
        .date_modified
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();

    let abs = engine.absolute_path_for(&filepath);
    let bucket_name = job
        .bucket_name
        .clone()
        .unwrap_or_else(|| "storage".to_string());
    let meta = ObjectMetadata {
        original_filename,
        filepath_uuid: filepath.clone(),
        file_format,
        filesize_bytes,
        etag_type,
        etag,
        date_uploaded,
        date_modified,
        bucket_name,
    };

    let stream = async_stream::stream! {
        yield ObjectChunk {
            content: Some(object_chunk::Content::Metadata(meta)),
        };

        match tokio::fs::File::open(&abs).await {
            Ok(mut file) => {
                let mut buffer = vec![0u8; 65536];
                loop {
                    match file.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(n) => {
                            yield ObjectChunk {
                                content: Some(object_chunk::Content::Payload(buffer[..n].to_vec())),
                            };
                        }
                        Err(e) => {
                            tracing::error!("read object for replicate: {e}");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("open object for replicate {}: {e}", abs.display());
            }
        }
    };

    let resp = client.replicate_object(stream).await?.into_inner();
    if !resp.success {
        anyhow::bail!("replicate failed: {}", resp.message);
    }
    Ok(resp)
}

pub async fn replicate_delete(
    client: &mut ReplicationClient,
    job: &OutboxJob,
) -> Result<ReplicationAck> {
    let filepath_uuid = job
        .filepath_uuid
        .clone()
        .or_else(|| job.filepath.clone())
        .context("missing filepath_uuid for DELETE")?;
    let etag = job.etag.clone().unwrap_or_default();
    let resp = client
        .delete_object(DeleteRequest {
            filepath_uuid,
            etag,
        })
        .await?
        .into_inner();
    if !resp.success {
        anyhow::bail!("delete replicate failed: {}", resp.message);
    }
    Ok(resp)
}

pub async fn ping(
    client: &mut ReplicationClient,
    node_id: &str,
    wireguard_ip: &str,
) -> Result<NodeHeartbeatAck> {
    let ack = client
        .ping_node(NodeHeartbeat {
            node_id: node_id.to_string(),
            wireguard_ip: wireguard_ip.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
        })
        .await?
        .into_inner();
    Ok(ack)
}

pub async fn sync_digest(
    client: &mut ReplicationClient,
    prefix: &str,
) -> Result<DigestResponse> {
    let resp = client
        .sync_digest(DigestRequest {
            bucket_or_prefix: prefix.to_string(),
        })
        .await?
        .into_inner();
    Ok(resp)
}
