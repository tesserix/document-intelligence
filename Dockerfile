ARG RUST_BUILDER=rust:1.94-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2
ARG TESSERIX_RUNTIME=debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
FROM ${RUST_BUILDER} AS build
WORKDIR /src

# `rust:* -slim` deliberately excludes the native build toolchain.  The
# managed-provider and Temporal dependency graph compiles native code, so keep
# the compiler available only in this disposable build stage.
RUN apt-get update \
    && apt-get install --no-install-recommends -y \
        build-essential \
        libprotobuf-dev \
        pkg-config \
        protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release \
    -p ocr-service --bin ocr-service \
    -p ocr-service --bin document-intelligence-mcp \
    -p ocr-parser-sandbox --bin ocr-parser-sandbox \
    -p ocr-temporal --bin ocr-dispatch-worker --bin ocr-execution-worker

FROM ${TESSERIX_RUNTIME}
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libssl3 tini curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir /app \
    && chown 10001:10001 /app
WORKDIR /app
USER 10001:10001
LABEL org.opencontainers.image.source="https://github.com/tesserix/document-intelligence" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build --chown=10001:10001 /src/target/release/ocr-service /app/ocr-service
COPY --from=build --chown=10001:10001 /src/target/release/document-intelligence-mcp /app/document-intelligence-mcp
COPY --from=build --chown=10001:10001 /src/target/release/ocr-dispatch-worker /app/ocr-dispatch-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-execution-worker /app/ocr-execution-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-parser-sandbox /app/ocr-parser-sandbox
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s CMD curl --fail --silent http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/bin/tini", "--"]
EXPOSE 8080
CMD ["/app/ocr-service"]
