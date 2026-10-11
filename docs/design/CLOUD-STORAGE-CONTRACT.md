# Cloud storage contract

This contract describes the storage implementation introduced with migration
`0018_storage_versions`. S3 and Azure are locally protocol-tested; **real-cloud
IAM, signing and full OCR qualification remain unverified**. Released versions
before this change support GCS only. [Operator setup](../BRING-YOUR-OWN-STORAGE.md).

## Public API

The [v1 OpenAPI contract](../../contracts/v1/openapi.json) is unchanged. Reserve
an upload, PUT bytes using **all** returned `required_headers`, complete it, wait
for acceptance, create a job and retrieve its result. Cloud credentials, bucket
selection and object paths are not request fields. API HMAC identity is separate
from cloud workload identity.

Azure signed uploads additionally return `x-ms-blob-type: BlockBlob`. Clients
must not hard-code a content-type-only header list. The API rejects unexpected
headers, including an injected `authorization` header.

## Operator route grammar

The existing environment variables are the configuration contract; there is no
new profile-registration endpoint or JSON configuration loader.

| Resource string | Provider | Meaning |
| --- | --- | --- |
| `my-bucket` | GCS | Backward-compatible bucket name |
| `gs://my-bucket` | GCS | Explicit bucket name |
| `s3://my-bucket` | AWS S3 | Bucket in the process's configured AWS region |
| `az://myaccount/mycontainer` | Azure Blob | Container in the named storage account |

No query, fragment, credential, arbitrary HTTP URL or object prefix is permitted.
The service generates object keys from verified product/tenant scope and opaque
IDs. Resource strings identify containers; they are not signed URLs. Bare GCS
names and `gs://` are equivalent storage locations, but a recorded route string
is immutable: do not rewrite configuration for in-flight or retained jobs.

`QUARANTINE_BUCKETS`, `SOURCE_BUCKETS` and `PAGE_BUCKETS` use comma-separated
`product=resource` entries. Execution-worker `RESULT_BUCKETS` uses that same map;
API/MCP `RESULT_BUCKETS` is a comma-separated resource allowlist. All execution
maps must contain the same product IDs. Quarantine and source storage must be
distinct and use the same provider. Cross-cloud source promotion is rejected.

One process uses its configured cloud credential chain. Per-product arbitrary
credential injection is not supported. For mutually untrusted customers, use
separate deployments/identities and private resource grants.

## Immutable object references

SQL persists the resource string, generated key, opaque version, SHA-256 and
length. The provider is encoded in the resource string (bare names mean GCS).
Version tokens are visible ASCII, 1–1024 bytes, excluding the S3 `null` sentinel.
They are never interpreted as checksums. ETags are not SHA-256 values.

| Provider | Pinned version |
| --- | --- |
| GCS | Generation represented as a decimal string |
| S3 | Non-null VersionId; enable bucket versioning |
| Azure | Blob version ID; enable blob versioning on a supported account |

Read the exact recorded version and verify returned metadata, length and digest.
Never substitute the latest object. Missing/unversioned content fails closed.
Azure hierarchical-namespace configurations without blob versioning do not meet
this contract.

## Adapter guarantees

| Operation | Behavior |
| --- | --- |
| Issue upload | One generated object, HTTPS PUT, short-lived signature and explicit required headers |
| Complete upload | Bounded stream, SHA-256/length/MIME verification, record exact version |
| Inspect | Scan and parse the recorded version; preserve tenant/product scope |
| Promote on GCS | Existing generation-pinned, create-only GCS rewrite path |
| Promote on S3/Azure | Read pinned source, verify bounded bytes, conditional create-only write, then verify destination bytes/version |
| Existing destination | Read and compare full digest/length; accept identical replay, reject conflicting bytes |
| Publish pages/results | Create-only write and read-back verification before committing the durable locator |
| Storage failure | Dependency-unavailable/retry or a stable invalid/conflict outcome; never successful OCR with an unreadable artifact |

Uploads remain bounded by 100 MiB and canonical results by 16 MiB. Source
promotion remains bounded by 120 seconds; existing read/write limits remain in
place. Cross-cloud copies are not performed. S3/Azure source promotion reads the
source into bounded memory; provision worker memory/concurrency for the maximum
payload, not only typical images.

## Credentials

S3/Azure use the `object_store` SDK credential providers and refresh behavior.
The uploader sees only an object-scoped URL, never the role/account credential.
The URL's effective lifetime is limited by both its signature and the underlying
credential/delegation lifetime. Expiration can occur earlier than upload reservation.

Prefer AWS workload roles and Azure managed/workload identities. Fallback secrets
must be injected from OpenBao or the operator's secret manager, not committed or
sent through the OCR API. Custom SDK endpoints are operator-controlled settings,
not caller-supplied values; third-party S3-compatible services and sovereign-cloud
endpoints have not been qualified by this change.

Document AI still performs OCR. `DOCUMENT_AI_AUTH_MODE=metadata` preserves the
existing Google-hosted credential path; `application-default` uses Google's
credential library with `GOOGLE_APPLICATION_CREDENTIALS`, including supported
external-account federation. This allows configuration outside GCP without
changing the OCR request/result contract; it does not eliminate Google billing
or cross-cloud residency/egress considerations.

## Migration and rollout

Migration 0018 adds nullable opaque-version columns. Reads fall back to legacy
numeric generation columns; compatibility triggers populate both for numeric GCS
versions and check consistency. Existing rows need no bulk rewrite. State checks
accept either representation while preserving upload/acceptance completeness.
RLS and product/tenant lookup predicates are unchanged.

1. Back up and inspect the database; apply 0018 using the migration owner before new binaries.
2. Roll out every API, MCP, dispatch and execution process, retaining existing GCS routes.
3. Verify GCS upload-to-result, retries, result retrieval and Temporal replay.
4. Only then opt new deployments/routes into S3/Azure, after their cloud qualification.
5. Retain old routes, credentials and referenced versions until their jobs/retention obligations finish.

The migration has a bounded lock/statement timeout. A failed migration must not
be followed by a binary rollout. Old binaries do not understand cloud-qualified
routes or opaque versions. Downgrade refuses incompatible references; do not
force it or delete artifacts to bypass the check. GCS compatibility does not
permit arbitrary rollback after enabling another cloud.

## Hosted customer onboarding

This change enables operator-configured self-hosting. Hosted BYO-bucket onboarding
is a separate administrative boundary, not an API that accepts customer keys.
Before offering it, implement tenant-bound grants, ownership verification,
audited credential federation/bindings, canary validation, rotation and retirement.
A current product HMAC key is trusted to assert tenant scope; it must not be shared
with mutually untrusted customer backends. AWS cross-account roles additionally
need constrained trust and a unique external ID.

## Qualification matrix

| Check | Local coverage | Still needs a real account |
| --- | --- | --- |
| Strict resource parsing and provider selection | Yes | Region/resource ownership |
| Exact S3/Azure version on GET | Protocol fixtures | Cloud permissions and versioning configuration |
| Signed PUT URL and Azure blob header | Synthetic signing tests | Live upload using workload credentials |
| Conditional create and conflicting replay | Protocol fixtures for both providers | Real service race behavior |
| Opaque versions, legacy reads/writes and tenant isolation | PostgreSQL tests | Approved production migration |
| Existing OCR workflow behavior | Existing service/store and Temporal suites | New-cloud upload-to-result, quotas, costs and outage recovery |

Run the [three-image test](../../benchmarks/README.md#quick-test-three-images)
and negative tenant/authentication/retention tests on each real deployment before
calling it production-qualified. Local protocol fixtures are not cloud certification.
