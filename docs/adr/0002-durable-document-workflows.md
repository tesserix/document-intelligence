# ADR-0002: Durable document workflows

**Status:** proposed

## Context

A job crosses Postgres, object storage, parsers, OCR providers, validation, review routing, and webhooks; it can run for minutes and must recover at page granularity. A hand-written queue consumer would need durable state, timers, retries, cancellation, recovery, and versioning.

## Decision

Use Temporal as the orchestrator for workflows longer than two steps. Use Postgres plus an outbox for accepted job state and reliable workflow start/events. Store large payloads in GCS and pass immutable locators/digests through workflows. Use bounded page-group child workflows for large documents. Activities are idempotent and at least once; workflow ID derives from job ID.

## Alternatives

- Pub/Sub plus a custom state machine: rejected because it recreates orchestration, recovery, timers, cancellation, and deploy-safe workflow versioning.
- One task per whole document: rejected because page 184 failure would repeat successful work and a large job would monopolize a worker.
- Synchronous request processing: rejected beyond a bounded wait on the durable job because provider latency and large inputs exceed safe request lifetimes.

## Consequences

Temporal is a critical operational dependency for new work, while accepted jobs remain recoverable from the outbox during an outage. Workflow determinism/versioning and activity idempotency become mandatory. This adds platform cost that must be measured against the existing Temporal deployment before production approval.

