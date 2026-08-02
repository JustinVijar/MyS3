FROM rust:1.97-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY migrations ./migrations
COPY src ./src
COPY web-ui ./web-ui

RUN cargo build --release \
    && strip target/release/rust-s3-engine

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust-s3-engine /usr/local/bin/mys3

ENV STORAGE_ROOT=/data \
    WIREGUARD_BIND_ADDR=0.0.0.0:9000 \
    GRPC_BIND_ADDR=0.0.0.0:50051 \
    DISABLE_TUI=1 \
    RUST_LOG=info

VOLUME ["/data"]
EXPOSE 9000 50051

ENTRYPOINT ["mys3"]
CMD ["serve", "--bind", "0.0.0.0:9000", "--grpc-bind", "0.0.0.0:50051", "--storage", "/data"]
