ARG RUST_BUILDER=rust:1.88-slim-bookworm@sha256:38bc5a86d998772d4aec2348656ed21438d20fcdce2795b56ca434cf21430d89
ARG TESSERIX_RUNTIME=ghcr.io/tesserix/base-debian-runtime:20260829@sha256:039b7701b5a0d01b63794ce2892e3d9f067f18884a96f9236d07e28cef6e0a74
FROM ${RUST_BUILDER} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p ocr-service

FROM ${TESSERIX_RUNTIME}
COPY --from=build --chown=10001:10001 /src/target/release/ocr-service /app/ocr-service
EXPOSE 8080
CMD ["/app/ocr-service"]
