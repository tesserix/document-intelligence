ARG RUST_BUILDER=rust:1.94-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2
ARG TESSERIX_RUNTIME=ghcr.io/tesserix/base-debian-runtime:20260919@sha256:0d23c4cc3f5be86a1a20207828340a76de1e45bbc4cdc86ccfac2c74e565ee79
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
COPY --from=build --chown=10001:10001 /src/target/release/ocr-service /app/ocr-service
COPY --from=build --chown=10001:10001 /src/target/release/document-intelligence-mcp /app/document-intelligence-mcp
COPY --from=build --chown=10001:10001 /src/target/release/ocr-dispatch-worker /app/ocr-dispatch-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-execution-worker /app/ocr-execution-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-parser-sandbox /app/ocr-parser-sandbox
EXPOSE 8080
CMD ["/app/ocr-service"]
