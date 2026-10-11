# Bring your own storage

**Your buckets, your cloud account, your data.** Self-hosters supply storage and
credentials; an OCR API key does not grant access to Tesserix infrastructure.

## What works today?

| Deployment | Status |
| --- | --- |
| Your own private GCS buckets and Google Document AI processor | Supported by the current runtime; qualify your installation with the quick test |
| AWS S3 or Azure Blob Storage | Implemented in this change with local protocol tests; real-cloud qualification pending |
| Worker hosted outside Google Cloud | Opt-in application-default credentials for Document AI; external-account federation must be qualified in your environment |
| Connecting a customer's bucket to Tesserix-hosted OCR | No public onboarding or credential-registration API exists |

See the [multi-cloud design](design/CLOUD-STORAGE-CONTRACT.md) for the storage and migration requirements. Versions before migration 0018 are GCS-only. Use the examples below only with the corresponding new binaries and schema.

## 1. Create four private GCS buckets

Choose names in **your** project, in a region compatible with your processor and
residency requirements. Replace the example names below; GCS names are globally unique.

| Bucket | Contents | Writers | Readers |
| --- | --- | --- | --- |
| `example-ocr-quarantine` | Untrusted uploads awaiting inspection | Upload capability issued by the API | API and dispatch worker |
| `example-ocr-sources` | Accepted, verified original documents | Dispatch worker | Dispatch and execution workers |
| `example-ocr-pages` | Derived page artifacts | Execution worker | Execution worker |
| `example-ocr-results` | Canonical JSON results | Execution worker | Execution worker, job API and optional MCP service |

Use uniform bucket-level access and public-access prevention. Do not grant
`allUsers` or `allAuthenticatedUsers` access. Keep these buckets separate from
customer-facing media buckets. Configure lifecycle policies per bucket, including
noncurrent versions and soft-deleted objects; do not expire artifacts while jobs,
retries or customer result-retention obligations still need them.

The service pins object generations. Overwrite/deletion of an in-use generation
can make a job unrecoverable. Define backup, retention and account-deletion
procedures together; archival rules alone do not implement account deletion.

## 2. Assign workload identities

Use separate identities for the API, dispatch worker and execution worker.
Scope grants to your specific buckets and processor. Cloud credentials belong to
server processes, never the mobile app or browser.

| Identity | Required operations |
| --- | --- |
| Upload API / URL signer | Create quarantine objects through signed PUT; read quarantine objects for completion verification; sign the upload capability |
| Dispatch worker | Read pinned quarantine generations; create accepted-source objects; read an existing destination to verify idempotent promotion |
| Execution worker | Read pinned sources; create/read pages and results; invoke your Document AI processor |
| Job API / optional MCP | Read pinned result objects only |

For GCS, use narrowly scoped object create/read permissions, such as
`storage.objects.create` and `storage.objects.get`, on the corresponding buckets.
Metadata-backed signing also needs `iam.serviceAccounts.signBlob` on the specific
signing service account; enable the IAM Service Account Credentials API. Grant
that permission to the actual caller identity, not to every service account.
The signing identity must itself have permission for the signed object operation.
Avoid project-wide Storage Admin grants. Provision lifecycle/cleanup permissions
on a separate operator identity; normal OCR requests do not require bucket administration.

By default, the execution worker uses the Google metadata server directly for
Document AI credentials. Use a Google VM service account or GKE Workload Identity
with the required processor access (`documentai.processors.processOnline`). A
local `gcloud auth application-default login` alone is insufficient for that worker.
See [Google signing permissions](https://cloud.google.com/storage/docs/access-control/signing-urls-with-helpers#required_permissions)
and [Document AI access control](https://cloud.google.com/document-ai/docs/access-control).

## 3. Configure the processes

These examples contain bucket names only, **not secrets**. Product ID `example`
is an example registered backend, not an end-user ID. These GCS examples use
legacy bare names; `gs://bucket` is also supported by the new adapters.

**GCS API (`OCR_API_CAPABILITY=full`):**

```dotenv
QUARANTINE_BUCKETS=example=example-ocr-quarantine
RESULT_BUCKETS=example-ocr-results
```

**Dispatch worker:**

```dotenv
QUARANTINE_BUCKETS=example=example-ocr-quarantine
SOURCE_BUCKETS=example=example-ocr-sources
```

**Execution worker:**

```dotenv
SOURCE_BUCKETS=example=example-ocr-sources
PAGE_BUCKETS=example=example-ocr-pages
RESULT_BUCKETS=example=example-ocr-results
```

**Important:** API/MCP `RESULT_BUCKETS` is a bucket allowlist. Execution-worker
`RESULT_BUCKETS` is a `product=bucket` map. Source, page and result maps must cover
the same products. Use distinct bucket routes per product; do not change a route
while old jobs still reference it. This is operator-managed configuration, not a
customer-controlled upload field.

For PostgreSQL, Temporal, parser, scanner and processor settings, follow the
[complete process configuration](SELF-HOSTING.md#process-configuration).

### AWS S3

Enable versioning on four private buckets. Block public access and require TLS
with an appropriate `aws:SecureTransport` bucket policy. Grant the API
signing role `s3:PutObject` and `s3:GetObject` on quarantine keys; grant dispatch
`GetObjectVersion` on quarantine plus `PutObject`/`GetObject` on sources. Execution
needs `GetObjectVersion` on sources and create/read/version-read on pages/results;
the result API needs `GetObjectVersion` on results. Add only the corresponding KMS
permissions if your encryption policy requires them. No bucket-administration grant
is needed for normal OCR. Qualify exact IAM policies with your cloud administrator.

```dotenv
# API; inject AWS role credentials separately
AWS_REGION=ap-southeast-2
QUARANTINE_BUCKETS=example=s3://example-ocr-quarantine
RESULT_BUCKETS=s3://example-ocr-results
```

```dotenv
# Dispatch
AWS_REGION=ap-southeast-2
QUARANTINE_BUCKETS=example=s3://example-ocr-quarantine
SOURCE_BUCKETS=example=s3://example-ocr-sources
```

```dotenv
# Execution
AWS_REGION=ap-southeast-2
SOURCE_BUCKETS=example=s3://example-ocr-sources
PAGE_BUCKETS=example=s3://example-ocr-pages
RESULT_BUCKETS=example=s3://example-ocr-results
DOCUMENT_AI_AUTH_MODE=application-default
GOOGLE_APPLICATION_CREDENTIALS=/var/run/ocr/google-external-account.json
```

The SDK supports workload-role credential sources, including `AWS_ROLE_ARN` with
`AWS_WEB_IDENTITY_TOKEN_FILE`, and supported instance/container role providers.
Prefer those to long-lived access keys. If using temporary injected credentials,
provide `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN` through
your secret integration, never through these checked-in examples. Set the region
explicitly (`AWS_DEFAULT_REGION` is also recognized; the default is `us-east-1`).
Do not assume a local AWS CLI profile is loaded by this adapter.

### Azure Blob Storage

Create four private containers in a storage account supporting blob versioning;
enable versioning, require secure transfer (HTTPS) and disallow public blob access. Use managed/workload identity.
Grant narrowly scoped blob read/write data actions for the process roles in the
bucket table. The upload identity also needs permission to generate a user-delegation
key at the storage-account scope; use an appropriate Storage Blob Delegator grant
with container-scoped data access. Verify SAS delegation from the actual workload.
Hierarchical-namespace configurations without blob versioning are unsupported.

```dotenv
# API
QUARANTINE_BUCKETS=example=az://exampleaccount/quarantine
RESULT_BUCKETS=az://exampleaccount/results
```

```dotenv
# Dispatch
QUARANTINE_BUCKETS=example=az://exampleaccount/quarantine
SOURCE_BUCKETS=example=az://exampleaccount/sources
```

```dotenv
# Execution
SOURCE_BUCKETS=example=az://exampleaccount/sources
PAGE_BUCKETS=example=az://exampleaccount/pages
RESULT_BUCKETS=example=az://exampleaccount/results
DOCUMENT_AI_AUTH_MODE=application-default
GOOGLE_APPLICATION_CREDENTIALS=/var/run/ocr/google-external-account.json
```

For workload federation, the Azure SDK reads `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`
and `AZURE_FEDERATED_TOKEN_FILE`. Use your platform's projected token and identity
binding. Managed-identity environments can use the SDK's managed-identity provider.
A storage account access key is a broad fallback credential, not a recommended
customer integration; if unavoidable, inject `AZURE_STORAGE_ACCESS_KEY` only
through server-side secret management. Never send it to uploaders. Azure clients
must PUT all returned headers, including `x-ms-blob-type: BlockBlob`.

### Google OCR credentials outside GCP

`DOCUMENT_AI_AUTH_MODE` defaults to `metadata`, preserving current deployments.
For AWS/Azure-hosted workers, select `application-default` and mount an
operator-controlled external-account configuration at `GOOGLE_APPLICATION_CREDENTIALS`.
Configure the corresponding Google workload identity pool/provider and processor
permissions. Do not accept this file or its credential-source URLs from API callers.
The official Google credential library performs supported exchanges and refreshes;
tokens and credential files must stay out of logs and images. Document AI project,
location and processor settings remain required, and OCR still incurs Google charges.
No AWS/Azure account was available to validate a real federation exchange for this change.

## 4. Keep API secrets separate from cloud identity

| Credential | Held by | Purpose |
| --- | --- | --- |
| OCR HMAC key | Trusted product backend and OCR API | Authenticate a product and signed tenant scope |
| Cloud workload identity | OCR server processes | Access your buckets and OCR processor |
| Temporary signed upload URL | The uploader, for its short validity | PUT one reserved object using the returned headers |
| Database credentials | The appropriate server process | Access its database role |

Generate an independent cryptographically random key of at least 32 bytes per
trusted product backend. Store it in OpenBao, GCP Secret Manager, AWS Secrets
Manager or Azure Key Vault and inject it through your deployment's secret
integration. The OCR binary does not fetch arbitrary secret-manager references.
Do not commit values, print them in logs, or place them in command-line arguments.

The injected API setting uses this **format**, not literal placeholder credentials:

```text
OCR_WORKLOAD_IDENTITY_KEYS=<key-id>=<registered-product>:<hex-encoded-random-key>
```

During rotation, register a new key ID alongside the old entry, deploy the backend
with the new key, then remove the old entry after the allowed request/retry
window. Reload requires a controlled process rollout; it is not a public API.

The backend authenticates its user, derives the permitted tenant, and signs each
OCR request. A product key is trusted to assert tenant scope for that product:
**do not issue one shared product key to mutually untrusted customers**. Use
isolated deployments/products today; hosted tenant-bound grants require the
control-plane work described in the storage contract.

## 5. Upload through the same API

```mermaid
sequenceDiagram
    participant App as Customer app
    participant Backend as Trusted backend
    participant OCR as OCR API
    participant Bucket as Your private bucket
    App->>Backend: Authenticated upload request
    Backend->>OCR: Signed upload reservation
    OCR-->>Backend: upload_id, upload_url, required_headers
    Backend-->>App: Temporary upload capability
    App->>Bucket: PUT exact bytes and required headers
    Backend->>OCR: Signed completion request
    Backend->>OCR: Poll until accepted; create job
    Backend->>OCR: Poll job; retrieve canonical result
```

The uploader does not choose a bucket, object path or cloud role in the OCR
request. The service derives them from verified product/tenant scope and its
configuration. Never send OCR HMAC headers to the storage URL. Treat signed URLs
as credentials: do not log, cache publicly or forward them to other hosts.
For browser uploads, configure bucket CORS for only your approved origins,
required PUT method and returned request headers. CORS does not replace authentication.
Native clients and trusted server clients do not need browser CORS.

Use the [nine documented HTTP endpoints](SELF-HOSTING.md#http-endpoints),
[signing contract](SELF-HOSTING.md#signing-and-a-complete-test-request) and
[quick-test images](../benchmarks/README.md#quick-test-three-images).
No cloud-storage credentials are accepted in the request body.

## 6. Verify before onboarding users

Run the three-image quick test against your own installation: nutrition and
receipt results must be retrievable; text-free input should require review.
Also verify:

- A different authenticated tenant gets 404 for the upload, job and result.
- Expired signatures and upload URLs fail; retrying does not duplicate jobs.
- Changing uploaded bytes causes completion verification to fail.
- Missing storage/scanner permissions leave work retryable or rejected, never falsely accepted.
- Result retrieval still works after a worker restart and within your retention window.

The fixtures are synthetic, but tests incur provider/storage usage. Keep reports
private and retain failed cases. A healthy API alone does not prove a working OCR journey.
