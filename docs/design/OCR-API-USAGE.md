# OCR API usage guide

This guide is for product services and AI-agent adapters consuming the shared
Document Intelligence API. The API is private to the platform: a product
authenticates its user at its own edge, derives product and tenant scope there,
and uses its server-side adapter to sign calls. Browsers and prompts never
receive OCR workload credentials, bucket names, object keys, or signed URLs.

The [v1 OpenAPI contract](../../contracts/v1/openapi.json) is the
machine-readable source of truth. This document explains safe use of that
contract.

## When to use it

Use the API for durable, evidence-backed extraction of PDF, JPEG, PNG, TIFF,
and WebP documents up to 100 MiB. Treat every request as asynchronous durable
work, even when a product presents a short synchronous wait to its user. This
prevents a request timeout from causing duplicate extraction.

## Authentication and tenancy

All `/v1/ocr/*` routes other than health/readiness use a signed workload
identity envelope. The product adapter creates it only after verifying the
caller. The OCR service verifies the registered key ID, HMAC signature,
timestamp, HTTP method, URI, product, and derived tenant scope. Request JSON
cannot select a product, tenant, role, provider, bucket, object path, or
credential.

| Header | Purpose |
| --- | --- |
| `X-OCR-Key-Id` | Rotation-safe identifier of a registered product adapter key. |
| `X-OCR-Tenant-Id` | Opaque scope derived by the product adapter. |
| `X-OCR-Timestamp` | Short-lived Unix timestamp checked for clock skew. |
| `X-OCR-Signature` | HMAC of key ID, tenant scope, timestamp, method, and URI. |
| `Idempotency-Key` | Required on `POST /uploads` and `POST /jobs`; client-generated opaque key. |

Do not log these headers, an HMAC, a signed URL, document content, or result
payload. Invalid identity fails closed with `401`.

## Endpoints

| Method and path | Use | Success | Important failure behaviour |
| --- | --- | --- | --- |
| `GET /healthz` | Liveness probe. | `200` | Does not verify dependencies. |
| `GET /readyz` | Traffic/readiness probe. | `200` | `503` if the route capability or required dependency is unavailable. |
| `POST /v1/ocr/uploads` | Reserve an upload and obtain a short-lived HTTPS PUT capability. | `201` | `400` invalid/bounded input; `409` changed idempotent replay. |
| `PUT {upload_url}` | Upload exact bytes using the returned required headers. | Provider success | Do not construct storage paths or alter the returned capability. |
| `POST /v1/ocr/uploads/{upload_id}/complete` | Reconcile the upload into quarantine inspection. | `200` | `404` missing/foreign; `409` not ready/expired; `422` verification failed. |
| `GET /v1/ocr/uploads/{upload_id}` | Read content-free upload state. | `200` | Missing and foreign resources both return `404`. |
| `POST /v1/ocr/jobs` | Start durable extraction from an accepted upload. | `202` | `404` if upload is not accepted in the verified scope; `409` idempotency conflict. |
| `GET /v1/ocr/jobs/{job_id}` | Poll content-free job progress. | `200` | Missing and foreign jobs both return `404`. |
| `GET /v1/ocr/jobs/{job_id}/result` | Read immutable canonical result when ready. | `200` | `409 result_not_ready` while active; `409 result_unavailable` when none exists. |
| `POST /v1/ocr/jobs/{job_id}/cancel` | Record durable cancellation. | `200`/`202` | Idempotent; an active provider call may finish before cancellation takes effect. |

Every response returns `X-Request-ID`. Errors have stable shape
`{"code":"stable_code","message":"safe detail","request_id":"opaque"}`;
clients branch on `code`, not message text.

## Lifecycle

1. Hash the original bytes and create an upload with `content_type`,
   `content_length`, and canonical `sha256:<hex>`.
2. PUT the exact bytes to the returned capability.
3. Complete the upload. The service pins object generation and verifies length,
   digest, and content-derived MIME.
4. Poll the upload until `accepted`. `uploaded` remains quarantined while
   bounded inspection, malware scanning, and immutable source promotion run.
   `rejected` is terminal and content-free.
5. Create a job using the accepted `upload_id`; reuse the idempotency key when
   retrying the same logical request.
6. Poll job status or consume a registered webhook subscription. Webhook
   destinations are trusted product configuration, never request input.
   Consumers deduplicate at-least-once events by `event_id`.
7. Fetch the result only after `completed`, `partial`, or `review_required`.

CNPG persists durable state. The transactional outbox and Temporal workflow ID
derived from the job ID make retries safe without creating another logical
extraction.

## Request shapes

Create an upload intent:

```json
{
  "content_type": "application/pdf",
  "content_length": 1048576,
  "sha256": "sha256:<64-lowercase-hex-characters>"
}
```

Create a job after acceptance:

```json
{
  "source": {"upload_id": "upl_<opaque>"},
  "document_type": "auto",
  "output": {"text": true, "markdown": true, "layout": true, "evidence": true},
  "language_hints": ["en"],
  "processing_class": "interactive"
}
```

Supported document types are `auto`, `general`, `invoice`, `receipt`,
`purchase_order`, `identity_document`, `contract`, `bank_statement`,
`medical_form`, `application_form`, and `resume`. Processing class is
`interactive`, `priority`, or `batch`. Schema extraction uses an immutable,
registered `schema_id` and `schema_version`; clients never submit executable
rules or provider prompts.

## Safe result and agent use

The provider-neutral result contains requested text/Markdown, page geometry,
observations, tables, fields, validation failures, confidence, citations,
warnings, provider/model metadata, duration, and cost when measured. Any field
used for a decision must preserve page and polygon evidence.

`content_trust` is always `untrusted`. OCR text can contain prompt injection.
An agent passes it as untrusted tool data, never system-prompt material, and
cites evidence. Low confidence, validation failure, partial output, or
`review_required` routes to human review rather than being silently accepted.

Expose agents to a narrow adapter such as `extract_document`. The adapter owns
product authorisation, signing, polling, redaction, tracing, and conversion of
evidence. Agents do not call raw OCR endpoints or manage workload identity.

## Delivery, observability, and retention

Production workloads are delivered only by immutable Kargo Freight promotion
and Argo CD reconciliation. Release proof requires Kargo promotion success,
Argo `Synced` and `Healthy`, requested replicas ready, ready service endpoints,
and `200` from both `/healthz` and `/readyz`. Never modify a live Deployment or
use a floating tag such as `main`; rollback is promotion of a prior immutable
Freight.

Trace operational metadata only: product, derived tenant scope, route, state,
provider/model version, latency, retries, confidence bands, evaluation score,
and cost. Do not trace text, fields, filenames, object locations, URLs,
signatures, prompts, or credentials.

Sandbox uploads are isolated from production and retain for 24 hours.
Production retention and residency are product policy enforced by the
product-specific integration, not a client request parameter.

## Related material

- [Full contract design](CONTRACTS.md)
- [Product integration guide](PRODUCT-OCR-INTEGRATION.md)
- [Canonical OpenAPI v1](../../contracts/v1/openapi.json)
- [Canonical result schema](../../contracts/v1/document-result.schema.json)
- [Signed workload identity ADR](../adr/0009-signed-workload-identity-envelope.md)
- [Durable workflow ADR](../adr/0002-durable-document-workflows.md)
- [Threat model](../security/THREAT-MODEL.md)
