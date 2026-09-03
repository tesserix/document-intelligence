# Contract design

This is the review draft from which OpenAPI and JSON Schema are generated after approval. It does not grant backward-compatibility status before review issue #4 closes.

## Reusable consumption modes

The public boundary is product-neutral. Applications use the versioned HTTP API
through generated clients; AI agents use the narrow Australis tool, which calls
the same API. Consumers do not import OCR engine/model internals and never
receive database, object-storage, vector-store or cache credentials.

Product adapters supply verified identity, tenant/product policy, registered
extraction schemas and authorized document references. They do not fork the
canonical result, evidence, error, job or event contracts. New optional fields
are additive within `/v1`; a breaking contract receives a new edge version with
a migration window and consumer compatibility tests.

The service publishes SDKs only as generated/typed wrappers over the API. An SDK
version cannot silently select a provider, weaken evidence, bypass review or
change tenant identity. At least two product fixtures must pass the same contract
suite before the reusable integration is considered proven.

## Common rules

- OAuth/OIDC or workload identity supplies principal and tenant. Request JSON cannot set either.
- Mutations require `Idempotency-Key`; all responses return `X-Request-ID`.
- Errors are `{"code":"stable_code","message":"safe detail","request_id":"opaque"}`.
- Unknown JSON properties are rejected on commands. Response contracts evolve additively within v1.
- Opaque IDs are non-sequential. Cross-tenant and missing objects both return 404.
- Times are RFC 3339 UTC. Money/cost is `{currency, decimal}` or `{currency, minor_units}`.

## Create upload intent

`POST /v1/ocr/uploads` requires `Idempotency-Key` and accepts no filename,
object path, bucket, URL, tenant, product, provider, or credential:

```json
{
  "content_type": "application/pdf",
  "content_length": 1048576,
  "sha256": "sha256:..."
}
```

The hard size ceiling is 100 MiB. Declared content type is restricted to the
P0 formats but remains untrusted until content sniffing. The response contains
an opaque `upload_id`, `PUT`, a ten-minute HTTPS capability, its expiry, and the
exact required request headers. Product-to-quarantine-bucket selection comes
only from trusted runtime configuration. Bucket and object names never appear
in the response. Replaying an identical request returns the same reservation;
reusing the key for different input returns `409 idempotency_conflict`.

The upload object name is generated from validated product/tenant/opaque IDs.
The expected digest and length are authoritative admission conditions during
reconciliation, even if a caller omits an advisory storage header. Inspection
pins the exact observed object generation before promotion, so later writes
cannot change the bytes being processed.

After the PUT, `POST /v1/ocr/uploads/{upload_id}/complete` reconciles the object.
It has no request body and is naturally idempotent on the upload resource. The
service streams rather than buffers the object, verifies the declared byte
length and SHA-256, detects MIME from magic bytes, and records the exact GCS
generation. The response exposes only `upload_id` and `status: uploaded`; it
does not expose storage metadata. Missing and foreign uploads both return
`404 upload_not_found`; absent objects return `409 upload_not_ready`; expired
reservations return `409 upload_expired`; mismatches return
`422 upload_verification_failed`.

The GCS read completes before the short CNPG transaction. That transaction
conditionally records `reserved → uploaded` and one `ocr.upload.received.v1`
outbox event. A crash before commit is retried; a crash after commit replays the
stored generation without duplicating the event.

## Create job

```json
{
  "source": {"upload_id": "upl_01..."},
  "document_type": "auto",
  "output": {"text": true, "markdown": false, "layout": true, "evidence": true},
  "extraction": {
    "schema_id": "invoice-v3",
    "schema_version": "3.2.0"
  },
  "language_hints": ["en", "hi"],
  "processing_class": "interactive",
  "webhook_subscription_id": "whs_01..."
}
```

The source is exactly one service-issued `upload_id`, approved tenant-storage reference, or batch-manifest entry. It is never an arbitrary URL. Inline custom schemas are deferred until schema-size, keyword, ownership and versioning rules are approved; production requests should prefer a registered immutable schema ID/version.

For the implemented upload path, job creation performs a scoped database join
and succeeds only when the referenced upload is already `uploaded` for the same
verified product and tenant. A guessed, foreign, missing, expired-before-upload,
or still-reserved identifier returns `404 upload_not_found`. Subsequent malware
and parser inspection remains part of the job workflow and can still reject the
document before OCR.

The response is `200` only when `Prefer: wait=N` completed within the bounded server wait; otherwise it is `202`:

```json
{
  "job_id": "job_01...",
  "status": "accepted",
  "created_at": "2026-09-03T05:00:00Z",
  "status_url": "/v1/ocr/jobs/job_01...",
  "result_url": "/v1/ocr/jobs/job_01.../result"
}
```

Repeated requests with the same tenant/key and identical digest return the original job. Reuse with different input returns `409 idempotency_conflict`.

## Job state

States are `accepted`, `inspecting`, `processing`, `validating`, `cancelling`, `cancelled`, `rejected`, `partial`, `review_required`, and `completed`. Status includes page totals/completed/failed, bounded progress, stable reason/warning codes, timestamps, and result/review availability. It never includes extracted content.

Cancellation is a durable request. A successful response means cancellation was recorded, not that every external provider call stopped instantly. Repeated cancellation is safe. Terminal completed/rejected/cancelled jobs do not move backward.

`GET /v1/ocr/jobs/{job_id}/result` returns `409 result_not_ready` while work is
active and `409 result_unavailable` for terminal states that produce no result.
A ready result is read from the exact generation-pinned GCS object selected by
trusted service state. Before returning it, the service verifies the bounded
content length, SHA-256 digest, schema version, document ID, document version,
and the invariant that content trust is `untrusted`. Bucket names, object names,
generations, and cloud credentials are never exposed in the public response.

## Canonical result outline

```json
{
  "schema_version": "1.0",
  "document_id": "doc_01...",
  "document_version": "sha256:...",
  "content_trust": "untrusted",
  "status": "review_required",
  "pages": [],
  "text": {"plain": "...", "reading_order": []},
  "classification": {"type": "invoice", "confidence": 0.97, "calibration_version": "..."},
  "fields": {
    "total": {
      "value": {"currency": "AUD", "decimal": "1280.50"},
      "confidence": 0.96,
      "evidence": [{"page": 1, "polygon": [[0.70,0.80],[0.90,0.80],[0.90,0.85],[0.70,0.85]], "observation_id": "obs_..."}]
    }
  },
  "validations": [],
  "reliability": {},
  "processing": {},
  "warnings": [],
  "review": {"required": true, "reason_codes": ["critical_field_low_confidence"]}
}
```

Full page observations are immutable and content-addressed. Every structured value has evidence. Coordinates are normalized against the original page and accompanied by transform provenance in `processing`. Confidence dimensions are separate and carry calibration versions. Unsupported/unmeasured values are absent with a reason, never zero.

## Event envelope

```json
{
  "event_id": "evt_01...",
  "event_type": "ocr.job.review_required.v1",
  "occurred_at": "2026-09-03T05:00:12Z",
  "tenant_id": "ten_01...",
  "job_id": "job_01...",
  "document_id": "doc_01...",
  "status": "review_required",
  "result_version": "sha256:...",
  "trace_id": "..."
}
```

Events contain no OCR text, field values, object URLs, filenames, passwords, prompts, or signed result URLs. Delivery is at least once. Consumers deduplicate `event_id`. Webhooks sign timestamp + event ID + body digest and reject stale timestamps.

## Australis tool

`extract_document` accepts an authorized document/upload reference, type hint, output mode, registered schema reference, language hints, and evidence flag. It returns either a terminal normalized result or an opaque job/status resume contract. The tool cannot accept provider credentials, arbitrary callback URLs, tenant identity, system instructions, or tool permissions.

Australis marks all returned document content as ADK `tool_result`/`untrusted`; the agent can quote or reason over it but cannot promote it to system/caller trust.
