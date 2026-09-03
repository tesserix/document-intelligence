# Quality, routing, validation and review design

## Pipeline decisions

Quality assessment precedes paid OCR. Preprocessing preserves the original and emits derived artifacts plus an invertible transform manifest. Each stage may improve OCR input, but the original quality score and defects remain visible. A source below the unusable threshold is rejected or reviewed; it is never presented as “fixed.”

Per-page stages are orientation, crop/perspective, deskew, background/shadow, denoise, contrast/sharpen, resolution/blur, blank/duplicate detection, then OCR. Stages are profile-versioned and bounded. Duplicate removal records the retained page; blank removal is disabled where page position has legal meaning.

## Confidence model

Do not average unlike scores into false certainty. Report:

- input quality by page and document;
- OCR recognition by observation/page/document;
- classification candidates;
- extraction confidence per field;
- deterministic validation outcome;
- overall reliability derived by a versioned policy.

Provider confidence is calibrated against the golden corpus by provider/model, document class, language/script, acquisition type and quality band. Calibration version and cohort travel with the score. Missing calibration is `unknown` and cannot auto-accept a critical field.

Overall reliability is a routing decision aid, not a replacement for dimensions. A critical validation failure or missing evidence overrides a high average. Thresholds live in versioned tenant policy with platform-enforced safe minima.

## Provider decision

Phase 1 selects a Google processor/profile using document class, region, and tenant policy. Once a second provider exists, the deterministic route policy intersects:

1. tenant provider allowlist, residency and retention constraints;
2. required capabilities such as handwriting/table/Markdown;
3. calibrated cohort accuracy;
4. provider health/quota and latency class;
5. per-job and tenant spend remaining.

Fallback triggers are typed: unsupported capability, bounded retry exhaustion, low calibrated confidence on a critical field, or deterministic validation failure. The route stores policy version, considered candidates, decision codes and cost estimate without document content. Budget denial produces review/typed failure, not an unbounded expensive call.

## Deterministic validation

Validators run after normalization and do not ask a model to perform arithmetic or format checks. Initial registry includes required-field presence, data type/format, date ordering, expiry, currency consistency, subtotal + tax = total with explicit rounding policy, line-item sum, and tenant-approved identifiers such as ABN format/checksum.

Each finding has stable code, severity, field paths, evidence references, expected/observed safe metadata, validator version, and review effect. Sensitive values are not copied into logs or event descriptions.

## Review routing

A review task is created when a critical field is unknown/below threshold, a blocking validator fails, providers materially disagree, class is unknown, pages are partial, or input is incomplete/illegible. Tenant policy may request additional review but cannot weaken platform safety minima.

The review contract contains result/document versions, reason codes, priority/SLA, field paths, source page regions, and short-lived authorized image-tile access. It does not duplicate the whole document into the task queue.

The UI presents original page imagery and derived highlight side-by-side when transforms could alter geometry. Reviewers see provider value, normalized value, confidence dimension/calibration, validation findings, and cited region. Accessibility and keyboard-only correction are acceptance criteria for the separate review application.

## Corrections

Corrections append a new version with actor, tenant, reason, timestamp, previous value digest, new typed value, evidence adjustment, and result/schema versions. Original provider output remains immutable. Concurrent correction uses an expected version and returns conflict rather than overwriting.

A correction may complete a review and publish a new result-version event. It is not automatically training data. Evaluation/training admission requires explicit tenant consent, de-identification policy, legal basis, quality review, and a separately versioned dataset manifest.

## Required review measurements

- unsafe auto-accept rate, especially critical fields;
- review precision/recall and avoidable-review rate;
- reviewer agreement and correction rate by provider/cohort;
- evidence-region accuracy after transforms;
- time to review and SLA breach rate;
- fallback incremental quality versus incremental cost/latency;
- calibration error and drift by cohort.

Promotion fails if a candidate improves the overall average while regressing a protected language/document cohort beyond its tolerance.
