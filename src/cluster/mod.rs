pub mod anti_entropy;
pub mod grpc_client;
pub mod grpc_server;
pub mod outbox;
pub mod peer_manager;
pub mod worker;

pub mod proto {
    tonic::include_proto!("replication");
}
