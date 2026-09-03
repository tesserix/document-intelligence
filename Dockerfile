ARG RUST_BUILDER=rust:1.88-alpine3.22@sha256:9dfaae478ecd298b6b5a039e1f2cc4fc040fc818a2de9aa78fa714dea036574d
FROM ${RUST_BUILDER} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p ocr-service

ARG TESSERIX_RUNTIME=ghcr.io/tesserix/base-alpine-runtime:20260829@sha256:9325eed71e33202088518fa0f933dc45b6c9b7412d13efb912c52ccfa73bc839
FROM ${TESSERIX_RUNTIME}
COPY --from=build --chown=10001:10001 /src/target/release/ocr-service /app/ocr-service
EXPOSE 8080
CMD ["/app/ocr-service"]
