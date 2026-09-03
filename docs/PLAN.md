# Delivery plan

## Checkpoint 0 — approve the design

- Review requirements, scale assumptions, SLOs, HLD, threat model, data lifecycle, and ADRs.
- Resolve every production-blocking decision listed in `REQUIREMENTS.md`.
- Produce reviewed OpenAPI, canonical JSON Schema, event schemas, provider contract, review contract, and error/status registry.
- Create child issues from Australis issue #20 with owners, dependencies, acceptance tests, and rollout/rollback notes.

Exit: architecture and contracts are accepted; no production infrastructure is created.

## Checkpoint 1 — service skeleton and safe intake

- Rust API and engine-worker binaries, configuration, health, graceful shutdown, structured logging, OTel, and contract generation.
- Postgres migrations/repositories with RLS and foreign-tenant negative tests.
- GCS upload intent, content hash, MIME verification, malware/parser sandbox, input limits, and retention/deletion state machine.
- Temporal workflow with outbox start, idempotent create/cancel, page activity recovery, and failure-path tests.
- CI: format, Clippy with warnings denied, tests, build, dependency/secret/image scans, pinned actions, SBOM and artifact provenance.

Exit: a safe fixture can be accepted, inspected, split into pages, cancelled, recovered after worker failure, and deleted without calling an OCR provider.

## Checkpoint 2 — Rust OCR engine MVP

- Signed detector/recognizer model profile, pinned ONNX execution profile, normalized page/text/confidence model, evidence transforms, and typed engine failures.
- Deterministic extraction/validation for the first agreed document class.
- Signed webhooks/events, partial results, review routing, dashboards, SLO alerts, runbooks, and cost accounting.
- Golden corpus baseline and PR/nightly/release gates.
- Separate sandbox, train/calibration, development-evaluation and sealed-test
  identities/data paths; signed candidate model staging and reproducibility records.

Exit: Phase 1 acceptance criteria and non-production end-to-end evaluation pass; GitOps rollout and one-action rollback are proven.

## Checkpoint 3 — agent integration

- Add the provider-neutral `extract_document` tool and registry metadata in Australis.
- Build the document agent in `ai-agents` on Tesserix ADK 0.54.0 with untrusted content, citation, containment, budgets, async resume, and review escalation. Pin only the reviewed published release, never a branch or floating tag.
- Register the agent/tool/dataset/eval suite in DevAI; run mocked PR evaluation, live candidate comparison, trace review, and gated promotion.
- Publish a signed compatibility manifest pinning prompt, model/parameters, tools,
  retrieval/knowledge, safety, memory, OCR profile, result schema and evaluation versions.
- Evaluate prompt-cache layout, tenant-safe semantic/tool-result caches, cache
  invalidation and cold-cache behaviour without caching side effects.
- Admit production traces only through consent, redaction, classification, human
  or governed automatic labelling, poisoning/leakage review and dataset freeze.
- Prove each product/environment derives identity, endpoint, policy and trace
  credentials from verified workload configuration; request/tool/document data
  cannot select another product or environment.

Exit: the agent passes deterministic safety/grounding/tool-sequence tests, quality judge checks, latency/cost budgets, and trace completeness without prohibited content.

## Checkpoint 4 — external fallback and advanced routing

- Add Google Document AI or Mistral only for measured cohort gaps, not anticipated abstraction.
- Introduce the versioned routing policy and fallback budget after two real adapters exist.
- A/B and shadow evaluation, provider disagreement handling, and tenant policy controls.

Exit: candidate beats or complements the baseline on the declared cohorts without regressing safety, residency, latency, or cost gates.
