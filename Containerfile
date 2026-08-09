# syntax=docker/dockerfile:1

FROM node:22-bookworm AS web-builder
WORKDIR /app/webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci
COPY webui/ ./
RUN npm run build

FROM rust:bookworm AS server-builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV CARGO_TERM_COLOR=always
WORKDIR /app
COPY rust/Cargo.toml rust/Cargo.lock ./rust/
COPY rust/crates ./rust/crates
WORKDIR /app/rust
RUN cargo build -p web-server --release --features full

FROM debian:bookworm-slim AS local-embedding-runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/aos
COPY scripts/download-local-embedding.sh scripts/setup-onnxruntime.sh ./scripts/
RUN chmod +x ./scripts/*.sh \
    && ./scripts/setup-onnxruntime.sh --dir /opt/aos/runtime/onnxruntime \
    && ./scripts/download-local-embedding.sh --dir /opt/aos/models/fastembed

FROM ghcr.io/astral-sh/uv:0.12.0 AS uv-runtime

FROM node:22-bookworm-slim AS server-runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        libgomp1 \
        libssl3 \
        python3 \
        ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /data --shell /usr/sbin/nologin aos \
    && mkdir -p /data \
    && chown -R aos:aos /data
COPY --from=uv-runtime /uv /uvx /usr/local/bin/
COPY --from=server-builder /app/rust/target/release/web-server /usr/local/bin/web-server
COPY --from=local-embedding-runtime /opt/aos/models /opt/aos/models
COPY --from=local-embedding-runtime /opt/aos/runtime /opt/aos/runtime
COPY licenses/Qdrant-paraphrase-multilingual-MiniLM-L12-v2-onnx-Q.txt /usr/share/licenses/aos/
COPY --chown=aos:aos examples /data/examples
ENV AOS_LOCAL_EMBEDDING_CACHE_DIR=/opt/aos/models/fastembed
ENV ORT_DYLIB_PATH=/opt/aos/runtime/onnxruntime/lib/libonnxruntime.so
ENV LD_LIBRARY_PATH=/opt/aos/runtime/onnxruntime/lib
RUN web-server --warm-local-embedding /opt/aos/models/fastembed
USER aos
WORKDIR /data
EXPOSE 3001
VOLUME ["/data"]
CMD ["web-server", "--addr", "0.0.0.0:3001", "--data-dir", "/data"]

FROM nginx:1.27-alpine AS web-runtime
COPY docker/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=web-builder /app/webui/dist /usr/share/nginx/html
EXPOSE 80
