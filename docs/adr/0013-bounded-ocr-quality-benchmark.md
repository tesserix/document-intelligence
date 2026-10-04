# ADR 0013: Bounded OCR preparation and reproducible measurement

Status: accepted, 2026-10-04.

## Context and budget

Production's 11 synthetic single-page tests took 10–17 seconds end to end.
Peak traffic, growth, real-world accuracy and load-tested percentiles are not yet
established; this change does not increase worker concurrency or promise an SLO.
The existing managed path accepts at most 20 MiB per provider request and bounds
provider work to 30 seconds. The initial goal is to keep one provider call per
page, retain all seven previously readable variants, and improve the 17/20-field
low-resolution label without introducing wrong numeric values. One fixed corpus
run is 21 jobs, not a load test or a statistically representative accuracy study.

## Decision

The managed provider response is restricted to `text,pages.pageNumber,pages.lines`,
the only fields consumed by the service, using the documented Document AI
field mask. This reduces response payload without changing evidence.

Before its existing provider call, an execution worker may resize a single-frame
PNG/JPEG/WebP whose dimensions are both at least 64 and at most 640 pixels. The
existing disposable parser process uses Lanczos3 at exactly 3x. Maximum output
geometry is 1920x1920; input and process output are capped at 20 MiB, decoder
allocation at 64 MiB, and execution at five seconds. Native orientation metadata
other than NoTransforms skips preparation. PDFs, TIFFs, large and tiny inputs
retain their existing path. Scaling preserves normalized evidence coordinates;
page geometry and content identity remain tied to the accepted original.

Preparation is optional: timeout, malformed output or process failure falls back
to the original accepted bytes. No second provider call is introduced. Existing
Temporal retry ownership, tenancy, signed identity and immutable publication stay
in place. The child environment is cleared before execution so service environment
credentials are not inherited. The parser requires no new cloud credentials or
network integrations.
Source/provider/storage failures retain their existing bounded retry semantics.

A leaf observation below 0.6 confidence adds `ocr_low_confidence` and
`retake_sharper_image`. This is a conservative review rule, not a calibrated
probability of correctness. Empty OCR adds `ocr_no_text_detected` and
`retake_include_readable_document`. Incomplete nutrient rows request a sharper
image; missing panels with otherwise readable text request column headers.
Existing finalization routes validation failures to review and preserves partial
page outcomes. No low-confidence text or evidence is silently rewritten.

Stage logs contain only job ID, a fixed stage name, monotonic elapsed milliseconds
and `ok`, `error` or `cancelled`. They cover source reads, preparation, provider,
response parse, page storage, result storage and database publication. They can
be grouped into p50/p95 timing distributions without logging document content.
The live benchmark records client-observed upload, inspection, job wait and
result read times; these include network and polling overhead. They are not
provider-only latency or a throughput claim.

The versioned synthetic corpus uses frozen SHA-256-verified images and authored
references. The scorer rejects duplicate, missing and mismatched cases, includes
failed cases, bounds edit-distance work, and reports CER/WER, missing/wrong fields
and nearest-rank latency percentiles. Numeric field tolerance is 0.01. The
benchmark distinguishes review/partial outcomes from perfect recognition.

## Alternatives and rollout

Global sharpening, contrast changes and hard image-quality rejection are deferred
until the benchmark demonstrates acceptable false-positive rates. A second OCR
engine or quality retry would add cost and latency; the measured small-image
resize improved the failing label to 19/20 with one provider call, so another
provider is unnecessary for this release. No extraction-specific changes are made
to generic receipt/invoice text recognition.

There are no migrations, API field changes, new cloud resources or additional
provider calls. Local CPU/memory and one small timing event per measured stage
are the incremental costs. Roll out the existing image through sandbox then
production via Kargo. Verify the full fixed corpus before and after, tenant and
idempotency checks, timing logs and result retrieval. Existing service alerts
remain applicable; benchmark failures stop release acceptance. If regression is
observed, the rollback target is the previous GitOps image tag 170fcbf4dcf17984d6b54e864a7ae2762e055ff4.

Real camera photos, handwriting, reflective packaging, additional scripts and
larger load tests still require a separately curated and labelled dataset. This
corpus must not be described as validation of every image quality or language.
