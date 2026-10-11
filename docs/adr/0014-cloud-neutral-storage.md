# ADR 0014: Cloud-neutral object storage with immutable versions

Status: implementation under review; live S3/Azure qualification pending.

## Context and bounds

Self-hosters need their own GCS, S3 or Azure resources. Existing code and SQL used
numeric GCS generations throughout upload, inspection, sources, pages and results.
S3/Azure version IDs are opaque. The existing API already hides provider details.

Keep the current 100 MiB upload and 16 MiB result ceilings. Four concurrent maximum
uploads can require at least 400 MiB just for promotion buffers; operators must
budget scanner/parser overhead separately. As an illustrative sizing input,
10 jobs/minute × 1 MiB is about 14 GiB/day of originals before pages/results,
retention and versions. Actual traffic, growth and p99/availability targets are
not supplied; this change makes no throughput/SLO claim. Measure them per installation.
Existing bounded operation deadlines and retries are retained.

## Decision

Reuse `object_store`'s GCS, AWS and Azure implementations behind a shared client.
Keep existing bucket environment maps, adding strict `gs://`, `s3://` and
`az://account/container` resource notation. Bare names retain GCS behavior.
Provider credentials are process-scoped deployment configuration, never API input.
No new service, database or customer onboarding endpoint is introduced.

Expand SQL with opaque-version columns and compatibility triggers. Read legacy
GCS generations as text; retain numeric values for old GCS binaries. Preserve
RLS, idempotency, outbox transactions and immutable artifact publication. Route
strings are persisted with each object and must not be repointed for existing work.

Retain GCS's tested rewrite adapter. S3/Azure promotion uses verified bounded
source bytes and conditional create, then verifies an existing or newly written
destination. An ambiguous successful write can be retried safely; different
bytes fail with conflict. Storage outages leave work retryable rather than
marking an unreadable artifact successful. No network call is added inside a
Postgres transaction.

Use the official Google authentication library as an opt-in application-default
path for Document AI outside GCP. Keep metadata authentication as the default.
Google remains the recognition provider; this is storage portability, not an
offline OCR engine or automatic switch to Textract/Azure Document Intelligence.

## Alternatives

- Merely enable SDK features: rejected; numeric versions and GCS rewrite semantics remain incompatible.
- Rewrite the public API per cloud: rejected; upload capabilities already provide a neutral client contract.
- In-place bigint-to-text conversion: rejected; it breaks rolling GCS compatibility.
- Hash a version into an integer: rejected; it destroys exact-version identity and introduces collision/mapping risks.
- Hosted dynamic customer credentials: deferred; requires its own tenant-bound administration and threat model.

## Security and operational consequences

Assets are documents, results and cloud/API credentials. Threats include foreign
tenants, malicious uploads, compromised callers and misconfigured operators.
Verified product/tenant scope selects server-owned routes; callers cannot supply
credentials, fetch URLs or choose object keys. Quarantine, scanning and parser
limits remain required. The migration adds no RLS bypass or secret storage.

Enabling AWS/Azure increases dependency surface. Review lockfile, licenses and
vulnerability scans. Use workload identities; restrict SDK endpoint/credential
configuration to the deployment operator. Signed URLs are secrets and must not
be logged. Keep cloud and HMAC identities separate.

Costs include extra read/write operations for verified promotion, version storage,
provider OCR charges and possible cross-cloud egress to Document AI. Price these
in the selected regions rather than claiming a universal saving. Separate cloud
routes also create new IAM, backup and retention responsibilities.

Apply the expansion migration before binaries. Keep GCS routes until all processes
are upgraded. Do not enable S3/Azure before real-cloud qualification; no such
accounts are available for this development task. Rollback to GCS-only binaries
is allowed only while compatible references remain; the down migration refuses
opaque/cloud-qualified references. No production migration is authorized by the
local development tests.

See [the contract](../design/CLOUD-STORAGE-CONTRACT.md) for API, rollout and test requirements.
