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
content-free event. Malware, pixel/page, decompression, parser sandboxing, and
the GCS promotion adapter remain the next quarantine stage before OCR processing.

The malware adapter implements the bounded ClamAV `INSTREAM` protocol over a
loopback-only TCP connection to a separately sandboxed sidecar. It streams only
the recorded GCS generation, rechecks generation and length, caps chunks,
response bytes, total bytes and wall time, and treats unknown replies or scanner
failure as unavailable rather than clean. It is not yet started by a production
importer; outbox consumption and source promotion remain required before rollout.
