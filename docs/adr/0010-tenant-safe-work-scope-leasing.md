# Tenant-safe work-scope leasing

## Context

The reusable document service accepts up to 20 jobs per second and processes up
to 100 pages per second. Upload reconciliation and durable workflow dispatch
are asynchronous so the create API remains below its 300 ms p99 target. CNPG
uses forced row-level security for every document, upload, job, result and
outbox row. A shared worker must discover pending work without receiving a
cross-tenant table-reading role or `BYPASSRLS`.

The existing transactional outboxes are correctly scoped, but they cannot be
polled until a product and tenant scope is already known. Static tenant lists
would drift, and giving a worker unrestricted table access would violate the
service tenancy contract.

## Decision

CNPG stores a content-free directory keyed only by `(product_id, tenant_id)`
with `upload_pending` and `dispatch_pending` flags. When an upload-received
event or workflow-dispatch event is inserted, the same transaction marks the
corresponding scope pending. The scoped transition that accepts/rejects an
upload or delivers the last dispatch event clears its flag. No filename, object
path, digest, OCR text, metadata, document ID or result is placed in the
directory.

Three security-definer functions own the directory boundary:

- `ocr_register_work_scope` and `ocr_set_work_scope_pending` update flags in
  the caller's transaction.
- `ocr_claim_work_scopes` uses bounded `FOR UPDATE SKIP LOCKED` leasing over
  those flags and returns only opaque valid product and tenant IDs.
- `ocr_release_work_scope` releases only the live owner lease.

`PUBLIC` has no execute grant. GitOps grants the API identity only
registration/flag updates and the dedicated dispatch-worker identity only
claim/release as required by its deployment. Neither identity has `BYPASSRLS`
or direct grants on the directory table. The security-definer functions query
only that directory, never forced-RLS content tables. Once it has a claimed
scope, the worker opens ordinary scoped transactions for upload inspection and
job-outbox delivery; forced RLS remains
the authorization control for every content-bearing operation.

Leases last five minutes, are naturally recoverable after a crash, and claims
are bounded to 100 scopes. An active upload inspection lease and outbox lease
remain their own authoritative retry state. The directory is only a discovery
and mutual-exclusion mechanism, never a queue or source of truth.

## Consequences

If CNPG is unavailable, intake completion and dispatch delay safely; no job is
acknowledged without its source transaction committing. If a worker crashes,
the scope becomes eligible after the lease expires and the existing idempotent
inspection/outbox state resumes it. Duplicate discovery is harmless because
the scoped upload and outbox claims are idempotent. A release failure costs at
most one five-minute dispatch delay.

The table has one small row per active or previously active product/tenant
scope, rather than per document. At 100,000 scopes it remains a small indexed
CNPG table; it does not justify a separate queue, cache, or shard. This avoids
static configuration while retaining strict RLS for document data.

## Rejected alternatives

- Worker `BYPASSRLS`: rejected because a compromised worker could read every
  tenant's document metadata and content locators.
- Static tenant configuration: rejected because onboarding and offboarding are
  not transactionally coupled to actual work.
- A global outbox reader: rejected because it exposes event payloads and makes
  every future event schema a tenant-isolation risk.

## Rollout and rollback

The forward migration is additive. Existing API versions remain compatible
until their runtime identity is granted registration execute permission through
reviewed GitOps. The worker is deployed only after its distinct claim/release
identity and a synthetic cross-tenant smoke test are approved. Rolling back
the application is safe while the migration remains; dropping the migration is
only safe after all callers stop invoking the functions.
