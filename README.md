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
