# Tesserix Document Intelligence

Reusable, multi-tenant OCR and document-intelligence service for Tesserix products and AI agents.

> **Status:** Non-production implementation has started under issue #8. The repository is not deployable yet; production infrastructure, model/provider promotion, real datasets, and product rollout remain gated on the recorded design reviews.

The service owns safe document intake, image quality analysis, OCR, layout recovery, classification, schema-driven extraction, deterministic validation, evidence, confidence, provider routing, and human-review routing. It does **not** own agent reasoning or a review UI.

Products consume the same versioned HTTP API through generated clients. AI
agents consume the provider-neutral Australis tool backed by that API. Product
code never embeds the OCR engine or receives its GCS, CNPG, Qdrant or Valkey
credentials. Product-specific storage, prompts, policy and observability remain
consumer integrations around the shared service.

## Repository boundaries

| Repository | Ownership |
| --- | --- |
| `tesserix/document-intelligence` | OCR and document-intelligence runtime, API, workflows, provider adapters, result contract, service-level evaluations |
| `tesserix/ai-agents` | Document agents and agent workflows pinned to the published Tesserix ADK 0.54.0 release |
| `tesserix/australis` | Provider-neutral MCP/tool registration, shared grounding policy, citations, and cross-product integration |
| `tesserix/devai` | Isolated agent sandboxes, golden-suite execution, trace inspection, comparison, and promotion gates |

## Design record

- [Requirements](docs/REQUIREMENTS.md)
- [Design review tracker](docs/DESIGN-REVIEW.md)
- [High-level design](docs/design/HLD.md)
- [API and data contracts](docs/design/CONTRACTS.md)
- [OCR API usage guide](docs/design/OCR-API-USAGE.md)
- [Canonical v1 OpenAPI](contracts/v1/openapi.json)
- [Canonical v1 contract manifest](contracts/v1/manifest.json)
- [Quality, routing, validation, and review](docs/design/QUALITY-AND-REVIEW.md)
- [Rust OCR engine](docs/design/RUST-OCR-ENGINE.md)
- [Data architecture](docs/design/DATA-ARCHITECTURE.md)
- [Kora runtime storage integration](docs/design/KORA-RUNTIME-STORAGE.md)
- [Sandbox, evaluation and training data](docs/design/SANDBOX-EVALUATION-TRAINING.md)
- [Evaluation and tracing](docs/design/EVALUATION-AND-TRACING.md)
- [Agent development, evaluation, publication, and product runtime](docs/design/AGENT-DELIVERY-LIFECYCLE.md)
- [Threat model](docs/security/THREAT-MODEL.md)
- [Delivery plan](docs/PLAN.md)
- [ADR-0001: service and repository boundaries](docs/adr/0001-service-boundaries.md)
- [ADR-0002: durable document workflows](docs/adr/0002-durable-document-workflows.md)
- [ADR-0003: untrusted content and evidence](docs/adr/0003-untrusted-content-and-evidence.md)
- [ADR-0004: Rust-native OCR engine](docs/adr/0004-rust-native-ocr-engine.md)
- [ADR-0005: CNPG, Qdrant, and Valkey](docs/adr/0005-data-platform.md)
- [ADR-0006: Kora runtime storage as a consumer integration](docs/adr/0006-kora-runtime-storage.md)
- [ADR-0007: sandbox, training, and held-out evaluation boundaries](docs/adr/0007-sandbox-evaluation-training-boundaries.md)
- [ADR-0009: signed workload identity envelope](docs/adr/0009-signed-workload-identity-envelope.md)

## Design checkpoint

Before implementation, reviewers must confirm the launch scale, residency regions, retention defaults, supported identity issuer, review-application owner, Google Document AI processor locations, Temporal hosting model, and the quality/cost thresholds in the evaluation plan.

## Current runtime configuration

`DATABASE_URL` is required. `RESULT_BUCKETS` is a comma-separated allowlist of
product/environment result buckets. `QUARANTINE_BUCKETS` maps verified product
IDs to isolated upload buckets as comma-separated `product=bucket` entries.
Both adapters use Application Default Credentials and GKE Workload Identity;
service-account key files are not accepted as application configuration. A
missing adapter leaves liveness available for diagnostics but keeps readiness
at `503`.

`OCR_WORKLOAD_IDENTITY_KEYS` is required and is supplied only from Secret
Manager to the OCR workload. It maps each rotation-safe key ID to its registered
product and 32-byte-or-longer hexadecimal HMAC key as
`key_id=product:hex_key`. Product adapters hold only their own key and sign the
tenant, timestamp, method, and URI after authenticating the user session. The
service rejects missing, malformed, stale, or mismatched envelopes; see
[ADR-0009](docs/adr/0009-signed-workload-identity-envelope.md).

`VALKEY_URL` optionally enables the degradable job-status cache. Cache entries
are schema-, product-, tenant-, and job-scoped; contain only status and creation
metadata; and always expire. Defaults are 10 seconds for active jobs, 300
seconds for immutable terminal jobs, a 25 millisecond operation timeout, and a
512-byte record limit. These bounds can be lowered or raised within hard limits
using `JOB_STATUS_CACHE_ACTIVE_TTL_SECONDS`,
`JOB_STATUS_CACHE_TERMINAL_TTL_SECONDS`,
`JOB_STATUS_CACHE_TIMEOUT_MILLISECONDS`, and
`JOB_STATUS_CACHE_MAXIMUM_RECORD_BYTES`. PostgreSQL remains authoritative and
every miss, invalid entry, timeout, or Valkey error falls through to the same
product- and tenant-scoped database lookup.

Run `scripts/test-valkey-cache.sh` for the digest-pinned real-Valkey contract
check. It verifies an expiring scoped round trip and bounded rejection of an
oversized value without coupling the reusable PostgreSQL CI job to Valkey.

Tracing is JSON-only when `OTEL_EXPORTER_OTLP_ENDPOINT` is absent. When set, the
endpoint must be a loopback collector or a fully qualified Kubernetes
`*.svc.cluster.local` gateway and `DEPLOYMENT_ENVIRONMENT` must be canonical.
The service refuses `OTEL_EXPORTER_OTLP_HEADERS` and
`OTEL_EXPORTER_OTLP_TRACES_HEADERS`: product-specific Langfuse credentials
belong only behind the product's telemetry gateway, never in this shared OCR
workload. The exporter uses OTLP/gRPC with bounded connect/export timeouts and
flushes on graceful shutdown.

`POST /v1/ocr/uploads` accepts only declared MIME, byte length, and canonical
SHA-256 plus `Idempotency-Key`. It issues a ten-minute HTTPS PUT capability for
an opaque service-generated object. The caller must send the returned headers.
`POST /v1/ocr/uploads/{upload_id}/complete` streams the object from its isolated
bucket, pins its exact GCS generation, hashes outside the async executor, and
checks byte length plus content-derived MIME before recording one durable
outbox event. Reconciliation leaves the object in `uploaded` quarantine state;
it cannot start a job. The acceptance store records the immutable promoted
source locator and a second content-free outbox event atomically, and only that
`accepted` state can start a job for the same verified product and tenant.
Inspection uses a five-minute, tenant-scoped CNPG lease: duplicate delivery by
the same worker renews it, another worker is excluded, an expired lease is
reclaimable, and ten exhausted attempts atomically reject the upload and emit a
content-free event. Acceptance also persists the bounded page count, maximum
and aggregate pixel counts, and parser profile/version; document text never
enters upload metadata or events. Importer orchestration remains the next
quarantine stage before OCR processing.

The malware adapter implements the bounded ClamAV `INSTREAM` protocol over a
loopback-only TCP connection to a separately sandboxed sidecar. It streams only
the recorded GCS generation, rechecks generation and length, caps chunks,
response bytes, total bytes and wall time, and treats unknown replies or scanner
failure as unavailable rather than clean. It is not yet started by a production
importer; outbox consumption remains required before rollout.

The source-promotion adapter uses GCS rewrite with the exact quarantined source
generation and destination `ifGenerationMatch=0`. Its destination is a
tenant/product-scoped SHA-256 path. A replay after an ambiguous successful copy
streams and hashes the existing destination before returning its pinned
generation; mismatched bytes fail closed. The adapter is not yet wired to a
production importer.

`ocr-parser-sandbox` is a separate no-network parser executable and image. It
accepts document bytes only on standard input, verifies PDF or declared image
format, bounds encoded bytes, PDF objects, pages, per-page pixels, and aggregate
render pixels, and emits only a small JSON metadata report. Malformed input,
password protection, and limit violations use stable content-free exit codes.
The importer-side process adapter now adds a bounded stdin/stdout protocol, a
two-minute hard ceiling on its configurable deadline, kill-on-timeout, strict
metadata decoding, and stable invalid/limit/password/unavailable outcomes.
Production use still requires the reviewed disposable runtime profile.

The importer coordinator now composes the short CNPG lease transaction with
exact-generation scanning and reading, bounded parsing, create-only promotion,
and a final atomic acceptance or permanent rejection. Dependency outages leave
the lease recoverable for bounded retry; foreign scope, stale ownership, source
conflicts, password requirements, and hard parser limits fail closed without
placing document content in database events.

Job workflow dispatch now uses a tenant/product-scoped CNPG outbox lease with
`SKIP LOCKED`, a 100-event batch ceiling, five-minute crash recovery and a
20-attempt dead-letter bound. The relay derives `ocr-job-{job_id}`, dispatches
start or cancellation outside the transaction, and acknowledges only with the
same live lease. An ambiguous Temporal start is therefore replayed with the
same workflow identity instead of creating duplicate execution.

## Contributing and license

This is an open-source project released under the
[Apache License 2.0](LICENSE). See [CONTRIBUTING.md](CONTRIBUTING.md) for the
development workflow, [SECURITY.md](SECURITY.md) for how to report
vulnerabilities, and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for community
expectations.
