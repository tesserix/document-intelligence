# ADR 0015: Public OCR capability sandbox

## Decision

Serve a small static UI and a server-side API adapter from one GKE service at
`ocr-test.tesserix.app`. GitHub Pages alone cannot hold workload credentials or
perform durable cleanup. The adapter calls the existing OCR upload/job APIs;
it never calls a chat model, JEV or a generative extraction service.

Benchmark limits: PDF/PNG/JPEG/TIFF/WebP, one document per session, 20 MiB,
up to 20 PDF pages, 12 megapixels per page and 240 megapixels per document.
The 20 MiB size cap matches the managed OCR path. Existing limits remain ten
sessions per network per hour, 100 uploads per UTC day across the deployment,
60 API requests per session per minute and a 15-minute session lifetime.
The largest admitted workload is 2,000 PDF pages/day and 2,000 MiB/day of original
uploads. Page tasks may read the same source repeatedly, increasing traffic.
Counters are shared in Postgres and admission fails closed on database outages.
The targets for small single-page samples remain 99% demo availability and p95
completion within 60 seconds; multi-page benchmarks require separate measurement.
These are targets, not SLAs or load-test results. Price the page/byte ceiling
against the Document AI tariff, traffic and pod/database footprint.

## Isolation and API

Use a dedicated `ocr-sandbox` product, OCR database, Temporal queue and four GCS
buckets. Never give this service Kora/Roamie credentials or bucket access.
`POST /v1/sandbox/sessions` issues a random, short-lived bearer capability. Its
SHA-256 hash is stored; each session gets a server-generated tenant. Every OCR
object lookup is scoped to that session's stored upload/job ID. Client-supplied
tenant/product headers are discarded. The adapter signs requests server-side.
Existing `/v1/ocr/uploads`, completion/status, job creation/status/result and
cancel routes retain their request/result shapes. Signed cloud URLs are never
sent to visitors; upload bytes pass through a bounded same-origin PUT endpoint.

The assets are visitor documents, API signing credentials and the OCR budget.
Threats include anonymous automation, cross-session access and hostile files.
The browser-to-adapter, adapter-to-OCR and adapter-to-storage boundaries validate
input separately. The private OCR service retains its malware/parser checks.
All intake reuses the existing parser subprocess with a five-second deadline and
a cleared environment. Apply the page/pixel limits in the child and validate its
returned report. Apply a 64 MiB allocation budget before image header inspection
and raster decode. Reject PNG metadata exceeding a cumulative 1 MiB encoded/decoded
budget before the image decoder can ignore oversized ICC profiles. Admit one
upload body per process before buffering; enforce 20 MiB on content and 64 KiB on
metadata. The browser reads `/limits.json`, and every server upload independently
checks length, SHA-256 and canonical content type. Malformed, encrypted, corrupt
or over-budget files fail closed; parser unavailability returns 503. The core
quarantine/ClamAV gate remains mandatory. Unsupported image types can be selected
and receive an explicit error. TIFF processing currently covers its first image;
use PDF for multi-page tests. No database migration, credentials or retention
policy changes are required. Rollback restores the adapter image through GitOps;
cleanup remains running. The sandbox runtime CI scan rejects HIGH/CRITICAL OS or
library findings, including unfixed issues; Rust dependency checks remain in CI.


No session token, signed URL, image or OCR text is logged. The frontend renders
text as text, never HTML. No localStorage, analytics or third-party UI scripts.

## Cleanup and failures

Session metadata is committed before any upstream upload. The adapter's cleanup
worker polls recorded jobs even when the browser disconnects. On terminal jobs,
it removes quarantine and accepted images, verifies absence, and records the
deletion time. The result route also attempts cleanup before claiming deletion.
Failures remain pending and are retried; the UI must not say "deleted" without
confirmation. Expired jobs are cancelled and allowed to reach a terminal state
before storage cleanup; an unavailable OCR API never permits speculative deletion
while workers might still be reading or writing. Page/result artifacts expire
with the session. Metadata is retained briefly for rate limits and recovery.

Dedicated buckets must have versioning, retention locks and soft deletion
disabled. Verify policy before deleting and delete every listed generation.
Lifecycle expiry is only a backstop, never evidence of immediate deletion.
The original image and OCR result remain visible in the visitor's browser until
they clear or leave the page; server cleanup cannot erase a user's local file.
Provider processing is governed separately by the OCR provider's policy.

## Verification and rollout

Test quota races, capability isolation, upload validation, upstream failures,
cleanup retries/restarts and generation deletion. Bundled samples have known
references and deterministic scoring; arbitrary uploads receive no invented
accuracy percentage. Package the adapter separately from the production OCR
binary. Apply its additive schema and provision isolated resources before a
separate GitOps rollout. Removing the public route stops admission; keep the
cleanup worker running until outstanding sessions are drained. No production
OCR schema or product retention policy changes are required.
