# ADR 0008: Temporal and CNPG retry authority

## Status

Proposed. Human design review is tracked in GitHub issue #27.

## Context

The OCR runtime accepts up to 20 jobs/s per region, with bursts to 50 jobs/s and 1–300 pages per job. At an assumed ten pages per job, workers must sustain 200 page attempts/s before burst headroom. Job-control operations target 99.9% monthly availability and p99 below 500 ms, excluding document processing. Accepted CNPG workflow metadata has an RPO of zero. A 300-page document must not make one failed page discard successful page artifacts.

Temporal provides durable orchestration, cancellation, worker recovery and bounded history. `PageWorkflow` in CNPG already owns page claims, deterministic activity keys, attempt counts, terminal state and optimistic revisions. Allowing both systems to count page-provider retries would create two independent limits and ambiguous recovery after a timeout.

The trust boundary crosses Temporal, a worker process, CNPG, object storage and an OCR provider. Activity payloads and histories must therefore contain identifiers and bounded status metadata only. OCR text, page bytes, signed URLs, extracted fields and secret values must never enter workflow history, logs or traces. Every database operation remains scoped by validated tenant and product identity under RLS.

## Options considered

1. Let Temporal retry each page and remove CNPG attempt state. This makes workflow history the business authority but loses the existing transactional checkpoint and partial-result model.
2. Let Temporal and CNPG both retry page failures. This was rejected because the effective attempt count becomes their product and duplicate delivery can exceed the agreed provider budget.
3. Keep page attempts in CNPG and use Temporal only for orchestration and transport recovery. This preserves one business retry authority and the existing artifact checkpoints.
4. Replace Temporal with an outbox consumer. This is simpler for a single pass, but does not meet the required cancellation, long-document continuation, worker-version routing and replay qualification needs.

## Decision

Choose option 3.

One identifiers-only runner activity invokes `CheckpointedPageRunner::run_once`. A retryable provider outcome is recorded in CNPG and returned to Temporal as a successful activity result with `running` metadata. Temporal then schedules another bounded runner iteration; only CNPG advances the page attempt.

Temporal retries an activity only when the durable transition is unknown or unavailable, such as a database timeout, connection loss or optimistic revision conflict. Validation, scope and missing-workflow errors are non-retryable. Activity redelivery is safe because CNPG revisions and deterministic page activity keys reject stale writes, while artifact and result publication are idempotent.

When CNPG reports `completed` or `partial`, a finalizer activity reads durable page locators, assembles the provider-neutral result in the worker process, writes it to object storage and atomically commits its locator. Temporal receives status and locator metadata only. Cancellation must durably mark the page workflow before workflow closure, after which no new page can be claimed.

Workflow code remains deterministic and continues as new before 50 runner iterations. Worker build/version routing protects histories during rollout.

## Dependency failure behaviour

- CNPG unavailable: the activity fails retryably with bounded exponential backoff; provider work does not start without a durable claim.
- Object storage or OCR provider unavailable: the CNPG page attempt records the bounded failure; independent pages continue and the result may become partial.
- Temporal unavailable: workers reconnect and accepted histories remain durable; new starts fail after the five-second RPC deadline.
- Final publication interrupted: a redelivered finalizer reuses durable artifacts and the existing commit result.
- Valkey, Qdrant or Langfuse unavailable: correctness is unaffected; cache, memory or telemetry degrades independently.

## Consequences

The activity boundary needs explicit error classification and metadata-only wire types. Live Temporal plus PostgreSQL tests must prove exhaustion, duplicate delivery, cancellation and cross-scope denial. A 300-page job can require up to 900 CNPG-owned page attempts at the current three-attempt policy, so queue depth and provider latency, not workflow history, drive worker sizing.

The incremental cost is Temporal activity traffic and CNPG checkpoint writes. Page and result payloads stay in object storage, avoiding Temporal payload-storage cost. The 24-hour qualification soak will establish measured history size, memory, p95/p99 latency and cost before production sizing.

## Delivery and rollback

The bridge ships as a separately versioned worker on a distinct task-queue route. Delivery progresses through sandbox, replay, shadow and canary stages. Rollback routes new starts to the previous compatible worker; the candidate worker remains available to drain histories it owns. No destructive schema change is required.

## Not included

This decision does not select an OCR provider, change production credentials, deploy Kubernetes resources, implement a review UI or claim completion of the required 24-hour soak.
