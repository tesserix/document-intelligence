# ADR-0016: Native document processing without AI

Status: Accepted for implementation; production rollout requires separate approval.

## Context and requirements

OCR execution currently sends every page to Google Document AI. The service must stop using learned models while preserving the v1 result shape, tenant isolation, source citations, existing upload formats, checkpoints, and review states. Arbitrary image text recognition is outside the capabilities of this policy: image-only pages must explicitly require manual review.

Observed demand is 243 pages over roughly 40 days. Growth requirements are unknown. The initial validation target is 1,000 pages and 100 MiB per document, with two simultaneously parsed documents per worker and four page activities. This is a bounded capacity target, not a throughput guarantee or a new production SLO. Uploads are writes; status and result reads reuse existing storage. At the present volume, retained data is small; neither sharding nor a vector database is justified. Future 12/36-month storage and peak demand must be measured before changing retention or capacity.

## Decision

Use native PDF text and layout extraction through Poppler's pdftotext in the existing disposable, credential-free parser process. Preserve page geometry and source-derived line bounding boxes. Pin the container base and ship the native parser plus its runtime libraries and license notices. No model weights, inference server, GPU, embedding generation, Qdrant, or pgvector is required.

Recognize the accepted document once, not once per page. Coalesce concurrent requests for the same product/tenant/job; retain only bounded extracted results, never source bytes indefinitely. A PostgreSQL header and per-page JSONB rows persist the canonical extraction under product/tenant/job, source digest and processing profile. Forced RLS and scoped foreign keys protect this derived cache; no text vectors are stored. Bounded in-memory entries coalesce requests, and cache pressure waits for a slot within the 75-second recognition deadline. Parse failures do not remain cached. Restarted workers load persisted pages rather than repeating source downloads or parsing. Concurrent publications reuse the first immutable canonical result. The durable page workflow and object artifacts remain the authority, allowing completed pages to survive worker restarts. No cross-tenant content deduplication is introduced.

Keep image upload support, with empty observations and explicit manual-review guidance instead of a cloud fallback. Mixed PDFs preserve all pages and supported text, flagging every page with no usable text for review. Validate XML size, page count, finite coordinates, decoded text, and parser output. A 60-second parser deadline kills the entire child process group, including Poppler; the recognizer has a 75-second budget inside the unchanged 120-second Temporal activity timeout.

Raise page-count constraints together across domain, parser, storage, SQL, Temporal input, and JSON schema. Retain the 100 MiB input limit; bound parser JSON/page/result artifacts to 64 MiB and Poppler layout XML to 128 MiB. PostgreSQL JSONB whitespace expansion has a separate 128 MiB stored-layout budget. Finalization checks aggregate declared page-artifact lengths before downloads. PDF text extraction does not rasterize pages; its cumulative theoretical pixel budget must accommodate 1,000 ordinary pages. Actual raster image decode limits stay bounded. Larger output fails with a stable limit outcome rather than exhausting memory.

CNPG stores job state, versioned artifact locators, tenant scope, and temporary native page text/coordinates. Successful result publication clears derived native pages; deletion of a job cascades to the cache. Failed/cancelled jobs retain their cache until job retention or an approved cleanup. Object storage holds document/page/result artifacts. Rendering reads those existing artifacts. Semantic vector search is not part of processing or rendering.

## Compatibility, failures, and rollout

Keep v1 fields and existing page observation shapes. Do not change the Temporal workflow's activity sequence, 50-iteration continue-as-new boundaries, or 3,000-iteration terminal ceiling. Keep that ceiling independent of upload limits to preserve replay of existing histories. The default eight-page batch fits 1,000 pages and ten attempts within that ceiling; very small batches and prolonged transport recovery can still reach it. Existing managed-provider library compatibility may be retained, but the execution binary must exclusively construct the native recognizer and must not initialize Document AI credentials or use its settings.

A parser timeout/crash is retryable; invalid/password-protected/over-budget input is a permanent, classified failure. Image-only or empty PDF pages are successful inspection outcomes requiring review, not endlessly retried recognition failures. Checkpointed successes are retained. Resumed legacy observations are marked `legacy-mixed`/`unknown` and require review; absent native metadata also triggers legacy review. The elapsed job age at native preparation is persisted once so retries cannot drift artifact checksums. After publication, replay returns the existing result without downloading or rewriting it. Store failures may retry artifact publication, with recognition reused while cached.

Apply migrations 0019, 0020 and 0021 in order before deploying the new worker. 0020 copies existing application-role page-artifact grants to the new tables and sequence. Test it on a disposable local database only. Downgrade in reverse order must refuse when records exceed the previous limits or when native cache rows remain. Never silently delete those records to force rollback. Production migration, rollout, and UI re-enablement are separate approved operations. The public UI remains at zero replicas.

## Validation and cost

Use test-first native PDF fixtures, multi-page/mixed/rotated documents, malformed and over-budget input, supported image formats, parser process cancellation, and real database compatibility tests. Verify repeatable text/order/citations, zero external AI calls, cache isolation/coalescing/recovery, and 1,000-page processing. Run formatting, Clippy, builds, affected full suites, and a real parser CLI exercise.

There are no inference-provider or GPU charges in this path. CPU, object storage, CNPG, and observability remain billable; precise incremental cost and throughput require representative load measurements. Synthetic host validation preserved 60,000 lines across 1,000 pages in about 12 seconds; this does not establish production p95/p99 or multilingual accuracy. Two documents can retain up to 200 MiB of encoded sources, 256 MiB of XML, and 128 MiB of serialized page results before decoded structures and database/client copies; worker memory must allow substantially more than that sum. Start isolated capacity tests at 2 vCPU/4 GiB per execution worker, measure peak RSS and queue latency, then right-size through GitOps. The old 512/768 MiB workload limits are not qualified for the enlarged envelope. A general-purpose learned OCR model would offer broader scanned-document automation but conflicts with the strict no-AI requirement. Plain PDF string extraction alone was rejected because it loses the existing spatial evidence contract.

This change applies to this execution worker only. Roamie downstream JEV/Gemini calls are separate consumer behavior and remain outside this implementation. Existing deployed workers still use their previous code until a separately approved rollout.

Offline Linux container validation preserved 60,000 lines from a 3.27 MiB synthetic 1,000-page PDF, producing 22.72 MiB of page JSON under a 1 GiB/2 vCPU limit in approximately 29 seconds including Docker startup/sampling. Sampled parser memory reached approximately 104 MiB; sampling is not peak-RSS measurement and does not qualify the full 100 MiB envelope.

Security implementation: a checksum-pinned minimal Poppler source build omits
HTTP, fontconfig, TIFF writing and raster decoders. Original source, Debian
patches and build recipe are bundled for redistribution. Linux parser children
inherit a fail-closed seccomp network/ptrace/process-memory restriction with
`no_new_privs`; parent service connections are unaffected. Release images must
pass the HIGH/CRITICAL vulnerability gate before publishing.
