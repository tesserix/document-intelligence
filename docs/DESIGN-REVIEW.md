# Design review tracker

Implementation is gated by recorded approval in GitHub issues. Silence, an emoji, or a merged unrelated change is not approval.

| Area | Review issue | Required perspectives | Status |
| --- | --- | --- | --- |
| Umbrella and sequencing | [#1](https://github.com/tesserix/document-intelligence/issues/1) | Product and architecture | Open |
| Requirements, capacity, SLO and cost | [#2](https://github.com/tesserix/document-intelligence/issues/2) | Product, SRE/platform, finance, data owner | Open |
| Runtime, workflow, storage and failure modes | [#3](https://github.com/tesserix/document-intelligence/issues/3) | Architecture, Temporal/platform, data/storage | Open |
| API, result, evidence and event contracts | [#4](https://github.com/tesserix/document-intelligence/issues/4) | Service, Australis, ai-agents, review UI, security | Open |
| Security, tenancy, residency and lifecycle | [#5](https://github.com/tesserix/document-intelligence/issues/5) | Security, privacy/data, platform | Open |
| OCR quality, routing, validation and review | [#6](https://github.com/tesserix/document-intelligence/issues/6) | Domain/product, ML quality, review owner | Open |
| Evaluation, tracing and DevAI verification | [#7](https://github.com/tesserix/document-intelligence/issues/7) | OCR evaluation, ai-agents, DevAI, observability, security | Open |
| Phase 1 implementation | [#8](https://github.com/tesserix/document-intelligence/issues/8) | Blocked on #1 design gates | Blocked |
| Rust-native OCR engine and runtime | [#9](https://github.com/tesserix/document-intelligence/issues/9) | Rust, ML inference, Temporal/platform, security | Open |
| CNPG, Qdrant and Valkey boundaries | [#10](https://github.com/tesserix/document-intelligence/issues/10) | Data, CNPG/platform, retrieval, security | Open |
| Kora runtime document storage integration | [#11](https://github.com/tesserix/document-intelligence/issues/11) | Kora, storage, service integration, privacy | Open |
| Shared sandbox, evaluation and training boundaries | [#12](https://github.com/tesserix/document-intelligence/issues/12) | Document Intelligence, ML/training, DevAI, security, privacy, platform | Open |

The cross-repository author → DevAI evaluation → signed publication → product
runtime sequence is documented in
[`docs/design/AGENT-DELIVERY-LIFECYCLE.md`](design/AGENT-DELIVERY-LIFECYCLE.md)
and is reviewed under #7, #8, #9, and #12.

Cross-repository integration has these additional owning issues:

- [tesserix/ai-agents#25](https://github.com/tesserix/ai-agents/issues/25) — application integration, redaction, failure isolation, rotation, and trace tests.
- [tesserix/tesserix-k8s#913](https://github.com/tesserix/tesserix-k8s/issues/913) — Kora Workload Identity, Secret Manager IAM/projection, egress, residency review, and GitOps rollout.
- [tesserix/tesserix-k8s#917](https://github.com/tesserix/tesserix-k8s/issues/917) — Kora development Langfuse resources, environment-derived routing, mutual dev/prod IAM denial and GitOps projection.
- [tesserix/tesserix-k8s#914](https://github.com/tesserix/tesserix-k8s/issues/914) — Kora runtime GCS buckets, IAM, lifecycle, recovery, and cost review.
- [tesserix/tesserix-k8s#916](https://github.com/tesserix/tesserix-k8s/issues/916) — product-neutral sandbox, training, evaluation, golden, model-staging, CNPG and protected-runner infrastructure.
- [tesserix/devai#392](https://github.com/tesserix/devai/issues/392) — compatibility-set evaluation, protected golden runs, agent safety/grounding gates, trace review, and promotion evidence.
- [tesserix/devai#393](https://github.com/tesserix/devai/issues/393) — reusable per-agent datasets, production-trace curation, deterministic/judge evaluations, release manifests and improvement flywheel.
- [tesserix/ai-agents#26](https://github.com/tesserix/ai-agents/issues/26) — signed agent compatibility manifests, prompt-cache-safe assembly and protected trace payload references.
- [tesserix/australis#21](https://github.com/tesserix/australis/issues/21) — tenant-safe semantic/tool-result caching, OCR digest reuse, authorization and invalidation contracts.

## Review evidence

Each design issue records:

1. reviewer name and responsibility;
2. decision: approve, approve with named follow-up, or request changes;
3. assumptions and measurements used;
4. rejected alternatives and consequences;
5. unresolved risk with owner and due date;
6. links to the reviewed document revision and any supporting evaluation/trace.

When a decision changes, update the owning ADR and re-open affected contract and consumer reviews. The service, Australis tool, and agent are promoted as a tested compatibility set.
