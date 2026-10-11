ARG RUST_BUILDER=rust:1.94-slim-bookworm@sha256:cf9dd0ec73e75f827fe59123fff9dc65af1a1c8363c3c31ee8d7f8ad0b6a5fb2
ARG TESSERIX_RUNTIME=gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
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
    -p ocr-service --bin ocr-service --bin ocr-healthcheck \
    -p ocr-service --bin document-intelligence-mcp \
    -p ocr-parser-sandbox --bin ocr-parser-sandbox \
    -p ocr-temporal --bin ocr-dispatch-worker --bin ocr-execution-worker

FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f AS init
RUN apt-get update && apt-get install --no-install-recommends -y tini \
    && rm -rf /var/lib/apt/lists/*

FROM ${TESSERIX_RUNTIME}
COPY --from=init /usr/bin/tini /usr/bin/tini
COPY --from=init /usr/share/doc/tini/copyright /usr/share/doc/tini/copyright
COPY LICENSE /licenses/document-intelligence.LICENSE
WORKDIR /app
USER 10001:10001
LABEL org.opencontainers.image.source="https://github.com/tesserix/document-intelligence" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build --chown=10001:10001 /src/target/release/ocr-service /app/ocr-service
COPY --from=build --chown=10001:10001 /src/target/release/document-intelligence-mcp /app/document-intelligence-mcp
COPY --from=build --chown=10001:10001 /src/target/release/ocr-dispatch-worker /app/ocr-dispatch-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-execution-worker /app/ocr-execution-worker
COPY --from=build --chown=10001:10001 /src/target/release/ocr-parser-sandbox /app/ocr-parser-sandbox
HEALTHCHECK --interval=30s --timeout=4s --start-period=10s CMD ["/app/ocr-healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--"]
COPY --from=build --chown=10001:10001 /src/target/release/ocr-healthcheck /app/ocr-healthcheck
EXPOSE 8080
CMD ["/app/ocr-service"]
