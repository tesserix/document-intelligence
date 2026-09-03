# Temporal Rust qualification

Status: non-production candidate; approval remains open in issue #9.

## Decision boundary

The OCR service keeps CNPG as the authoritative job lifecycle and transactional outbox. Temporal receives only versioned opaque identifiers after the outbox commit. PostgreSQL, GCS, OCR-provider, callback and result I/O remain activities; workflow code must not perform I/O or read process state.

The candidate targets 20 jobs/s and 100 pages/s peak, a 300-page document ceiling, six workflow runs of at most 50 pages, and a five-second client RPC deadline. Each page is independently addressable so a failure on page 184 retries `ocr-page-0184`, not the other 299 pages.

## Supply-chain qualification

| Component | Pin | Cargo checksum |
| --- | --- | --- |
| `temporalio-client` | `=0.8.0` | `06a672ea7c8fb963e3da4224c74b4fb2b0d2085dbe55d9b735a24c70af68c68f` |
| `temporalio-common` | `=0.8.0` | `49a8ab419d730698e89a027a9c80aaedd8da40f8b4a761ad5bf94abfd62ccbfe` |
| `temporalio-macros` | `=0.8.0` | `43058fe5557a8d122704c136991510703ea3998ff83d5279d0e0924434164413` |
| `temporalio-sdk` | `=0.8.0` | `099245859a89a43c9218ab0e3e37fab62a3bcbd2fa962ef409c8c783ed0c8679` |
| `temporalio-workflow` | `=0.8.0` | `aa469b8e55b2ad6cc589b5596bd4ec4b2ac498800a38c5f1a69027d3fd751004` |

The corresponding upstream `v0.8.0` tag resolves to commit `207acc165c8091421a3eb41aef65b1ca53ae6aa1`. The Rust SDK is Public Preview. Exact Cargo versions and `Cargo.lock` checksums prevent dependency drift, but they do not make a preview SDK production-ready.

## Current evidence

- Workflow IDs hash validated `product_id`, `tenant_id` and `job_id` with separators, preventing cross-product and cross-tenant collisions while revealing no business content.
- Input schema `1` accepts only product, tenant and job IDs plus a page count from 1 through 300. Unknown fields are rejected and serialized input is limited to 512 bytes.
- Start uses Temporal's reject-duplicate workflow-ID policy. An already-started workflow is an idempotent success.
- Cancellation carries the outbox event ID as the Temporal cancellation request ID. A missing or already-closed workflow is an idempotent success.
- Client RPCs have a five-second deadline. Failures remain retryable at the outbox boundary, and an outbox row is acknowledged only after Temporal accepts the command.
- Page activity policy requires a 120-second start-to-close timeout, a 10-second heartbeat timeout, three attempts, and bounded backoff. Validation and scope failures are non-retryable.
- A 300-page plan continues as new after each 50-page run, bounding history growth.
- The SDK workflow executes one independently identified activity per page, propagates cancellation to the active activity, and continues as new with a strictly validated run number and next-page cursor.
- The qualification activity carries only scoped identifiers and a page number, heartbeats progress, checks cancellation before and after yielding, and returns page metadata only. It is not a production OCR implementation.
- Workflow inputs, IDs, search attributes, logs and traces must never contain document text, filenames, URLs, object locators, credentials or extracted values.

On 2026-09-04, `scripts/test-temporal-qualification.sh` ran against Temporal CLI 1.5.0 / server 1.29.0. A 51-page workflow completed through two runs and its first-run history replayed successfully with the candidate worker. A separate 300-page workflow was cancelled while its first page activity was executing; history proves Temporal requested activity cancellation, scheduled no following page and closed the workflow as cancelled. A third 300-page workflow ran its activity worker in an isolated process: the harness killed that process during page 1, started a replacement, observed page 1 complete on attempt 2 with the recorded prior failure, completed the other 299 pages once and finished after six bounded runs. The script downloads the exact platform archive, verifies a hard-coded SHA-256 copied from the official release checksums, uses `EphemeralExe::ExistingPath`, and deletes its temporary server files on exit. This is functional qualification evidence, not the required 24-hour soak.

The 0.8.0 high-level start API generates its own request ID and does not expose a caller-supplied start request ID. Workflow-ID reuse policy therefore provides start idempotency; the deterministic outbox request ID remains available for cancellation and audit correlation. This API limitation must be re-evaluated during upgrade qualification.

## Required integration matrix

The following evidence remains mandatory before production selection. The first worker-loss case is now automated; the remaining cases are open:

1. Completed locally: start a 300-page fixture, terminate the isolated activity-worker process during page 1, replace it, and prove page 1 alone advances to attempt 2 before all six runs complete.
2. Replay histories produced by a separately versioned current worker with the candidate worker and fail the gate on nondeterminism. Same-candidate first-run replay is complete.
3. Exercise cancellation before start, between page runs and after completion. In-flight page cancellation is complete.
4. Exercise bounded activity exhaustion and prove the job becomes partial/review-required without losing completed page artefacts.
5. Exercise signals and child-workflow behavior if either is introduced; neither is approved merely by this client adapter.
6. Prove worker-version routing with old and candidate workers active simultaneously.
7. Run the upgrade soak below for at least 24 hours and retain server/worker versions, history sizes, retries, latency and memory artefacts.

The integration harness pins and checksum-verifies the Temporal CLI release rather than allowing the SDK downloader to select an executable. It is manually invoked, uses disposable local storage, requires no cloud credentials and is excluded from ordinary unit tests. No unverified server image or executable is used.

## Twenty-four-hour upgrade soak

1. Start a digest-pinned disposable server and the current worker build.
2. Submit a deterministic mix of 1-, 50-, 51- and 300-page synthetic jobs, duplicates, cancellations and injected retryable failures for 12 hours.
3. Introduce the candidate worker using Temporal worker versioning while existing executions remain active.
4. Continue the same load for 12 hours, including worker kills and server restarts.
5. Replay sampled histories with both compatible builds.
6. Fail qualification for nondeterminism, lost or duplicated terminal state, unbounded history, cross-scope access, missed cancellation, raw-content telemetry, p99/SLO regression, or memory growth beyond the approved envelope.

## Rollback and fallback

This candidate is not wired into production startup, so rollback is removal of the isolated `ocr-temporal` adapter and restoration of the previous outbox consumer. If the preview SDK fails any gate, use a narrow stable Go Temporal runner that accepts the same versioned, content-free protocol and calls the Rust OCR activities. Do not replace Temporal with a custom durable state machine.

## Approval gate

No merge, deployment or production runtime selection occurs until Temporal/platform, security and SRE reviewers approve issue #9 with the integration and soak artefacts attached.
