# ADR-0005: CNPG Postgres, Qdrant semantic memory and Valkey cache

**Status:** proposed

## Context

OCR job state, document provenance, review corrections and audit records require transactional correctness. Agent retrieval needs vector search. Hot status/result reads, admission counters and deduplication hints need low latency. Binary documents and large normalized results do not belong in a relational row or vector payload.

Using all three datastores without strict ownership would create conflicting truth, incomplete deletion and outage coupling.

## Decision

- CloudNativePG Postgres is the authoritative transactional database.
- GCS stores immutable source, derived page and full result objects.
- Qdrant stores rebuildable embeddings and bounded searchable metadata for semantic document/agent memory.
- Valkey stores expiring cache entries, distributed admission counters and non-authoritative coordination hints.

No business fact exists only in Qdrant or Valkey. A canonical memory/chunk record and its source evidence exist in Postgres/GCS before indexing. Postgres transactions write business state and an outbox event together; idempotent consumers update Qdrant and invalidate/refill Valkey after commit.

## CNPG topology

Start with one three-instance CNPG cluster in the approved region, spread across zones: one primary and two streaming replicas. Use a dedicated database and least-privilege roles for API, worker, outbox/indexer and migration identities. Application traffic uses a CNPG `Pooler` in transaction mode; SQLx disables statement caching behind PgBouncer.

Continuous WAL archiving and scheduled base backups write to a separate versioned GCS backup bucket through Workload Identity. Initial targets are automatic failover within two minutes for a pod/node failure, backup RPO at most five minutes and full restore RTO at most 60 minutes. These are accepted only after quarterly restore and failover exercises.

Migrations are forward-only and GitOps delivered, with lock/statement timeouts and expand-migrate-contract sequencing. Backups do not make an incompatible schema rollback safe; the previous application version must remain compatible.

## Qdrant ownership

Use collections partitioned by region and embedding model major version. Every point carries tenant ID, canonical record ID, document/version, chunk/span/evidence IDs, schema version, retention deadline and embedding version. Queries require an authorized tenant filter at the repository boundary. Regulated or outsized tenants may receive a dedicated collection only by policy.

Point IDs derive deterministically from tenant, canonical record version and embedding version, making at-least-once indexing idempotent. Re-indexing builds a new versioned collection, validates recall/latency, switches an alias, and retains the prior collection for bounded rollback.

Qdrant is eventually consistent with Postgres. Index lag is observable. A Qdrant outage does not block OCR completion; indexing remains queued and semantic search returns a typed unavailable/degraded result rather than silently incomplete answers.

## Valkey ownership

All keys are tenant- and schema-version namespaced and have explicit jittered TTLs. Cache-aside reads fall through to Postgres/GCS. Terminal job states may use longer TTLs because they are immutable; active status is short lived. Writes invalidate through the outbox so a database commit is never dependent on Valkey.

Rate limits and quotas use atomic scripts or single-round-trip increment/expiry operations. If Valkey is unavailable, the service bypasses cached reads and applies conservative per-instance admission ceilings so the outage increases latency without allowing unbounded work.

Valkey is not used as the durable job queue, workflow history, document store or authoritative idempotency ledger.

## Deletion

Deletion begins with an authorized Postgres state transition and outbox event. Workers remove source/results from GCS, points from every Qdrant collection/version and related Valkey keys. The job becomes `deleted` only after required stores confirm removal; otherwise it remains `deletion_pending` with retries and alerts. A content-free tombstone/audit record remains according to policy, and backup expiry is disclosed separately.

## Alternatives

- Postgres with pgvector only: simplest and retained as a fallback, but Qdrant is selected for the requested semantic-memory specialization and independent vector scaling.
- Qdrant as memory truth: rejected because transactional audit, corrections, erasure and rebuildability would be weaker.
- Valkey as a queue or authoritative idempotency store: rejected because eviction/outage must not lose accepted work.
- Store OCR JSON and documents in Postgres: rejected because large payloads inflate WAL, backup and replication cost.

## Consequences

Each store has one clear failure mode and recovery path, but the platform operates three stateful systems. The added Qdrant and Valkey cost is justified only by measured retrieval and latency needs. GitOps, backup, restore, tenancy and observability must cover each system before production.

