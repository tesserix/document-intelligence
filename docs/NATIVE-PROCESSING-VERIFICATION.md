# Native processing findings and verification

Local review date: 2026-10-14. The implementation and security fixes are locally verified; the approved GitOps rollout is being prepared.
The public OCR UI returned HTTP 503 during final verification. Backend workers
retain their previously deployed behavior. UI shutdown alone does not prevent
other authorized consumers from submitting jobs or incurring provider charges.

## Findings and execution boundary

The original execution worker constructed `MetadataDocumentAiTransport` from
`DOCUMENT_AI_PROJECT_ID`, `DOCUMENT_AI_LOCATION`, and
`DOCUMENT_AI_PROCESSOR_ID`, and passed it to `DocumentAiPageRecognizer`.
That was an AI OCR route, including when the input already contained PDF text.

The updated worker constructs only `NativePageRecognizer` and enables native
finalization. It does not read Document AI settings or initialize that provider.
The managed-provider library remains for API/source compatibility, but is not
selected by this worker. Model-profile and historical hybrid design code should
not be mistaken for a currently selected runtime path.

Roamie's downstream JEV/Gemini usage is outside this change. Removing OCR's
Document AI route does not remove those consumer calls. The source inspection
establishes an AI dependency, but does not by itself attribute a specific Vertex
billing spike or prove what a `devai` console entry represents. Confirm that
attribution using billing SKUs, project, time interval, processor/model request
logs, and principal before making a claim about the exact charge.

## Implemented behavior

| Input or event | Outcome |
| --- | --- |
| PDF with usable embedded text | Native Poppler text/layout extraction with source line polygons and reading order |
| PDF containing text and scans | Preserve supported text and every page; empty pages require manual review |
| Image-only PDF, JPEG, PNG, WebP | Validated input, empty observations, explicit manual-review result |
| Multipage TIFF | Validate all frames and preserve each page for manual review |
| Bad decoding, malformed/password-protected/over-budget input | Classified rejection or review; no inference-provider fallback |
| Worker restart or repeated activity | Reuse canonical tenant/source/profile-bound PostgreSQL native pages |
| Retried finalization after publication | Return the existing result without rereading or rewriting it |
| Resumed legacy page artifacts | Legacy/unknown provenance and explicit review warning |

The service no longer attempts automated recognition of scanned text in this
path. Learned OCR, including neural Tesseract configurations and hosted OCR
models, would contradict the strict no-AI requirement. Native extraction can
preserve an inaccurate embedded text layer; confidence 1.0 denotes source
fidelity, not independent recognition accuracy. Office documents, handwriting,
arbitrary scripts, and every possible PDF encoding are not newly guaranteed.

## PostgreSQL, rendering, and determinism

CNPG stores tenant-scoped JSONB page text and coordinates in derived cache rows.
Forced RLS, composite scope foreign keys, accepted-source joins and source-digest
checks protect reuse. Neither Qdrant, pgvector nor embeddings are required.
Existing object artifacts and v1 citations remain the rendering contract.
No new review UI or semantic-search API is introduced.

Two in-memory document slots coalesce work. A full cache waits within a bounded
fallback extraction budget. Successful publication clears derived native rows;
failed/cancelled jobs retain them until job retention or approved cleanup.
Include this temporary plaintext in database residency, backup and retention
policy. There is no cross-tenant deduplication.

Canonical cache publication is immutable. The elapsed job age at preparation is
recorded once and reused, preventing duration/checksum drift within a job.
Text/order/coordinates repeat for identical sources and a fixed runtime image;
different job identities and timing metadata are expected to differ. Pin the
image digest and retain `/licenses/native-text.packages` for reproducibility.

## Bounds and compatibility

- 100 MiB source, up to 1,000 pages, 100 million pixels per page.
- 64 MiB native page JSON/result budget; aggregate page artifacts are checked
  before finalizer downloads. JSONB/layout XML is bounded separately to 128 MiB.
- 60-second parser deadline and 75-second cache/extraction budget inside the
  unchanged 120-second page activity timeout.
- Existing 50-iteration continue-as-new and 3,000-iteration terminal workflow
  boundaries preserved. The new replay-boundary regression failed before this
  was fixed. Keep the default eight-page batch; very small batches with many
  retries can reach the existing terminal ceiling.
- V1 fields, source citations, authentication, tenant boundaries, page activity
  names, successful checkpoints, object-version checks and review states remain.

Migrations 0019–0021 align SQL limits and add the native cache. Their up/down/up
sequence was exercised through the CI setup script on a fresh disposable local
PostgreSQL 17 instance. Downgrades refuse larger records/artifacts or outstanding
native cache rows. No production migration was run.

## Verification evidence

Commands actually run, with local test connection variables loaded from private
files rather than printed:

```sh
cargo fmt --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo check --workspace --all-targets --locked
cargo build --workspace --locked
cargo test --workspace --locked
cargo test --locked -p ocr-store -p ocr-service -- --ignored --test-threads=1
cargo test --locked -p ocr-temporal --test live_temporal_qualification -- --ignored --nocapture --test-threads=1
cargo test --locked -p ocr-temporal --test durable_postgres_bridge -- --ignored --nocapture --test-threads=1
cargo test --locked -p ocr-temporal --test work_scope_dispatcher -- --ignored --test-threads=1
cargo deny check licenses
docker build -f Dockerfile.parser -t document-intelligence-parser:native .
docker build -t document-intelligence-service:native .
bash scripts/smoke-images.sh document-intelligence-service:native document-intelligence-parser:native
python3 ci/native-text-smoke.py document-intelligence-parser:native
ruff check --no-cache ci/native-text-smoke.py
ruff format --check --no-cache ci/native-text-smoke.py
bash -n scripts/setup-test-database.sh scripts/smoke-images.sh
sh -n ci/install-native-text.sh
gitleaks git --pre-commit --redact --no-banner
gitleaks dir ci --redact --no-banner
gitleaks dir crates --redact --no-banner
gitleaks dir migrations --redact --no-banner
```

Workspace: 261 passing tests; real database service/store: 46 passing tests;
local Temporal: seven replay/restart/cancellation checks, two PostgreSQL bridge
checks and one work-scope dispatcher check passed. Temporal CLI 1.5.0 was pinned
and checksum verified. The existing qualification wrapper failed because the
host's `sha256sum` lacks GNU flags; the checks were run with a portable checksum
command and the same pinned CLI. No real-cloud storage qualification was run.

Final service and parser image builds passed with the pinned Rust 1.94 builder.
Container smoke checks passed non-root identity, valid/invalid parser input,
API liveness, dependency readiness, and unauthenticated denial. The parser
bundled in the service image also extracted the fixture offline/read-only.
Formatting, Clippy, typecheck, build and scoped secret scans passed.

The container test first failed against the image without Poppler with exit 13,
then passed offline/read-only with source text, coordinates and repeatable output.
A 1,000-page/60,000-line synthetic PDF produced 22.72 MiB of page JSON from a
3.27 MiB source under 1 GiB/2 vCPU in approximately 29 seconds including Docker
startup/sampling. Sampled memory reached approximately 104 MiB. This is not a
peak-RSS measurement, production latency guarantee, full 100 MiB stress test,
or multilingual/real-world accuracy benchmark.

## Security findings and fixes

`trivy image --scanners vuln --severity HIGH,CRITICAL --exit-code 1
 document-intelligence-parser:native` failed with 11 HIGH and zero CRITICAL
advisories after native dependency inventory was included. The final service
image scan reported the same 11 HIGH and zero CRITICAL findings. No fixed Debian
versions were listed:

| Library | Advisory IDs |
| --- | --- |
| libcurl3t64-gnutls 8.14.1-2+deb13u5 | CVE-2026-12064, CVE-2026-8286, CVE-2026-8458, CVE-2026-8927 |
| libexpat1 2.8.3-1~deb13u1 | CVE-2026-66046, CVE-2026-76956, CVE-2026-76957, CVE-2026-77214, CVE-2026-93990 |
| libtiff6 4.7.0-3+deb13u3 | CVE-2026-36849, CVE-2026-52490 |

`cargo audit` also failed with RUSTSEC-2026-0343, RUSTSEC-2026-0338 and
RUSTSEC-2026-0340 in `quinn-proto` 0.11.17; the recommended patched version is
0.11.18 or later. This version was already in HEAD's lockfile. The findings were
not suppressed, and an unrelated dependency upgrade was not bundled into this
change. Rust dependency license checks passed; native package licenses and
notices are retained, and Poppler redistribution/source obligations need review
before publishing images.

The original findings above are now resolved. `quinn-proto` is patched to
0.11.18 and `cargo audit` passes. The native image builds only `pdftotext` from
checksum-pinned Debian Poppler source 25.03.0-5+deb13u4. HTTP, TIFF writing,
fontconfig and raster decoders are disabled, removing all three affected library
groups. The corresponding source, Debian patches and build recipe ship with
the binary. The minimal parser scan reports zero HIGH/CRITICAL findings.

Linux parser children install an inherited seccomp filter after setting
`no_new_privs`. The filter denies network socket operations, io_uring setup,
ptrace and process-memory access, rejects alternate syscall ABIs, and fails
closed if installation fails. A real C socket probe through `ParserProcess`
failed before the fix and passed afterwards. Parent storage/database/Temporal
connections retain their existing behavior. This restriction is Linux-specific;
macOS development retains the existing process/time/output isolation.

The migration client is separately packaged without the unused `gosu` helper,
which had one CRITICAL and 24 HIGH advisories. Its final effective filesystem
scan is clean. Release publication now requires a clean runtime scan for each
image; there are no vulnerability suppressions.

A near-100 MiB synthetic source (104,857,598 bytes) with 1,000 pages and 60,000
lines passed the secured minimal parser under 1 GiB/2 vCPU in 9.33 seconds
including startup/sampling. Sampled memory reached 271.8 MiB. Padding accounts
for most source bytes; this checks the input-size envelope, not the complexity
of every real 100 MiB document.

Then benchmark representative documents at the full envelope, starting isolated
execution-worker capacity tests at 2 vCPU/4 GiB. Right-size from measured RSS,
CPU and queue latency; do not assume the old 512/768 MiB limits fit every input.
Native processing removes inference-provider charges only after worker rollout;
CPU, database, object storage and observability still cost money. No measured
monthly production cost estimate is available.

Apply migrations, promote workers through the owning GitOps repository, monitor
review/error/timeout rates, and reopen the public UI only through separately
approved production actions. Deployment approval was provided after this report's initial local-only review.
The rollout uses an isolated GitOps worktree, opt-in PreSync migration jobs,
advisory-lock serialization, transactional checksum records and staged sandbox
then production promotion. The real migration runner passed initial execution
and replay, and rejected a changed checksum. Database-owner credentials remain
secret references and are never printed. The public UI remains at zero replicas.
The deployed revision and final live verification will be recorded separately.
