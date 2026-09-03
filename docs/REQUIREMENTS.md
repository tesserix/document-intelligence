# Requirements

## Product outcome

One secure document-intelligence capability serves every Tesserix application. A caller receives normalized, cited data independent of the OCR provider. Low-quality or uncertain output is explicit and routable; it is never silently presented as correct.

OCR, semantic extraction, deterministic validation, review, and agent reasoning remain separate stages with separate quality measures.

## Launch assumptions to validate

These are capacity inputs, not promises:

| Dimension | Launch design point |
| --- | --- |
| Tenants | 100 active tenants |
| Monthly volume at 12 months | 100,000 documents / 400,000 pages |
| Monthly volume at 36 months | 500,000 documents / 2,000,000 pages |
| Peak intake | 20 jobs/s and 100 pages/s across all tenants |
| Typical document | 4 pages, 2 MiB |
| Large-document boundary | 300 pages or 100 MiB, whichever comes first |
| Interactive boundary | 1 document, at most 2 pages and 10 MiB |
| Result size | approximately 250 KiB/page before compression |
| Default raw/result retention | 30 days; tenant policy may shorten it |

At the 12-month point, 30-day retention is roughly 200 GiB of typical raw input plus 100 GiB of uncompressed normalized output. Binary and full result payloads belong in object storage; Postgres stores indexed lifecycle metadata, policy versions, result locators, validation summaries, and the audit trail. No sharding is justified at this scale.

## Service objectives

| Objective | Phase 1 target |
| --- | --- |
| Job API availability | 99.9% monthly |
| `POST /v1/ocr/jobs` p99 | under 300 ms, excluding upload |
| `GET /v1/ocr/jobs/{id}` p99 | under 200 ms |
| Eligible interactive document p95 | under 10 s end-to-end |
| Up-to-20-page asynchronous document p95 | under 120 s end-to-end |
| Accepted-job durability | no acknowledged job lost |
| Metadata RPO / RTO | at most 5 min / 60 min |
| Cross-tenant data exposure | zero tolerance |
| Critical-field result without evidence | zero tolerance |

Provider-side rejection, malicious input, cancellation, and quality-gate rejection are typed outcomes, not service availability failures. Accuracy promotion thresholds are dataset-specific and are defined in the evaluation plan rather than hidden inside an aggregate confidence number.

## P0 — production MVP

### Intake

- PDF, PNG, JPEG, WebP, and TIFF, including multi-page inputs.
- Mobile photographs and scanned documents.
- Service-issued upload, tenant-scoped object reference, and batch manifest.
- Password-protected PDFs through a one-time, short-lived encrypted secret token; passwords never enter job URLs, events, traces, or durable plaintext metadata.
- Content-based MIME verification, malware scan, parser sandbox, and page/pixel/size/time/decompression bounds.
- Orientation correction, deskew, perspective correction, crop, denoise, contrast, shadow/background handling, and resolution/blur assessment.
- Explicit quality score, warnings, and `input_unusable` outcome. Enhancement never disguises an unusable source.

### OCR and normalized output

- Document, page, block, paragraph, line, and word text where the provider supports it.
- Reading order, normalized coordinates, page dimensions, confidence, language/script, handwriting indication, tables, cells, headings, lists, headers, footers, and selection marks.
- A provider-neutral versioned result with provider/model/version, route decision, processing duration, warnings, validation findings, and measured cost.
- Evidence for every structured value: document version, page, normalized polygon/span, provider observation, and transform chain.

### Intelligence and validation

- `auto`, `general`, invoice, receipt, purchase order, identity document, contract, bank statement, medical form, application form, and resume classification.
- JSON Schema-constrained extraction with bounded schema size/depth and an allowlist of supported keywords.
- Dates in RFC 3339 where time exists or ISO 8601 calendar form where it does not; money as currency plus integer minor units or a decimal string, never a binary float.
- Separately reported input quality, OCR, classification, field, validation, and overall reliability.
- Deterministic validators for required fields, formats, date ordering, currency consistency, totals, and configured tenant rules.
- Review routing for low confidence, failed validation, provider disagreement, unknown type, incomplete image, or illegibility.

### Execution and integration

- One durable workflow path for all jobs. “Synchronous” means the caller waits for a bounded interval on that same job; it is not a second processing implementation.
- Idempotent create/cancel, page-level retry and recovery, partial results, cancellation, signed webhooks, and page/result events.
- Interactive, priority, and batch task-queue bulkheads with per-tenant quotas and concurrency.
- Tesserix's Rust-native OCR engine first, using signed and benchmarked model profiles. External OCR services remain comparison baselines or separately approved fallbacks.
- W3C trace context, tenant-safe structured logs, RED metrics, page/provider metrics, cost attribution, and append-only audit events.

## P1 — reliable agent service

- Mistral OCR fallback for complex PDF/Markdown use cases.
- Confidence- and validation-driven fallback with a tenant-configurable spend ceiling.
- Review API and correction provenance.
- Agent-facing `extract_document` MCP tool in Australis.
- Document agent in `ai-agents`, evaluated and promoted through DevAI.
- Prompt-injection screening, ADK untrusted envelopes, citation/grounding gates, and tool-containment tests.
- Side-by-side provider evaluation and version promotion gates.

## P2 — advanced platform

- Advanced Tesserix detector, recognizer, layout, table, handwriting and multilingual profiles; regional routing, translation, PII redaction, searchable PDF, comparison, splitting, barcode/QR recognition, duplicate/fraud indicators, feedback-driven evaluation, tenant glossaries, and versioned extraction templates.
- Multi-region active/active only after measured availability or residency requirements justify its operational cost.

## Explicit non-goals

- Agent reasoning or autonomous action inside this service.
- Treating a vision LLM as the default OCR path.
- Training on tenant documents or reviewer corrections without explicit consent and governance.
- An arbitrary URL fetcher. Remote ingestion is limited to service-issued uploads and approved tenant storage connectors.
- A generic workflow builder or a provider abstraction designed beyond providers actually implemented.
- A human-review user interface in this repository.

## Decisions still requiring an owner

| Decision | Why it blocks production |
| --- | --- |
| Launch and residency regions | Determines storage, provider processor, encryption, and failover topology |
| Default/max retention and backup expiry | Determines deletion truth and legal posture |
| Identity issuer and service-to-service audience | Required for object-level authorization |
| Review application owner | Needed before the review contract can be accepted |
| Golden-set licensing and sensitive-data governance | Real samples must be legally usable and safely retained |
| Per-document/page cost ceilings | Provider fallback cannot be safe without a budget |
| Quality thresholds by document class/language | One global threshold would hide weak cohorts |
