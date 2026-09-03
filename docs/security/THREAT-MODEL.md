# Threat model

## Assets, actors, and trust boundaries

Assets are raw documents, extracted PII/PHI/financial data, tenant encryption material, provider credentials, schemas/templates, reviewer corrections, audit history, and service capacity/budget. Threat actors include an unauthenticated internet client, an authenticated user from another tenant, a malicious tenant, a compromised document/provider/dependency, a webhook receiver, and an insider with excessive access.

Trust boundaries exist at every upload/object reference, API call, queue/workflow message, parser process, provider request/response, database/object lookup, webhook, review action, Australis tool call, agent context, CI fixture, and telemetry export. Every crossing validates structure and bounds, authenticates the caller where applicable, and authorizes the specific tenant object.

## Threats and required controls

| Threat | Control and verification |
| --- | --- |
| Cross-tenant IDOR | Derive tenant from verified identity; scope every lookup in the query/object path; RLS defense in depth; foreign-tenant tests return 404 |
| Malicious PDF/image parser exploit | MIME sniff, malware scan, patched sandboxed parser, no network, read-only FS, seccomp, CPU/memory/time/output limits, disposable process |
| Decompression/pixel/page bomb | Limits before and during decode; compressed-to-expanded ratio, pixel/page/time budgets; fail closed |
| SSRF through signed/source URL | Do not accept arbitrary URLs; service-issued conditional uploads and approved connector allowlists only |
| Path/object-key traversal | Generated opaque keys; resolved tenant prefix; never use supplied filename as path |
| Prompt injection in document | Mark result `untrusted`; ADK untrusted envelope and injection guard; content cannot alter principal, tenant, system directives, tools, callbacks, credentials, route policy, or schema |
| Schema/resource exhaustion | JSON Schema keyword allowlist, byte/depth/property/regex limits, compile timeout, version/digest pinning |
| Provider retention/residency leak | Tenant policy intersects provider capability; region-pinned processors; retention settings recorded; deny route when policy cannot be met |
| Secret/password leak | One-time encrypted token, KMS envelope, short TTL, consume-once, no URL/event/log/trace plaintext, deletion audit |
| Kora Langfuse credential crossing product boundary | Secret Manager access only for the named Kora AI Workload Identity; no OCR/Australis/DevAI access; both public/secret key resources handled as secrets; IAM denial tests and rotation proof |
| Dev/prod environment confusion | Environment and product derive from verified workload/deployment identity; distinct ServiceAccounts and secrets; mutual IAM deny tests; request, prompt and OCR content cannot select endpoints or credentials |
| Webhook forgery/replay | Per-destination secret reference, HMAC over timestamp/event/body digest, short acceptance window, event ID dedupe, HTTPS allowlist, rotation |
| Duplicate jobs/side effects | Required idempotency key, unique tenant key, Temporal workflow ID, idempotent page attempts, consumer event dedupe |
| Result/evidence tampering | Immutable object generation, SHA-256 digest, versioned transform manifest, signed locator, audit event |
| Cache tenant crossing | Tenant + document version + processing/profile/schema/provider versions in key; no shared unscoped cache key |
| Log/trace exfiltration | Allowlisted metadata fields, typed redacting values, no bodies/text/filenames/URLs, automated forbidden-attribute tests |
| Supply-chain compromise | Pinned dependencies/actions/images, lockfiles, provenance/SBOM/signing, secret scan, least-privilege CI, review automated updates |
| Denial of service/cost attack | Authenticated quotas, size/page/time bounds, tenant concurrency and spend budgets, admission control, bulkheads, circuit breakers |
| Reviewer abuse | Object/state authorization, reasoned corrections, append-only actor/time/before/after audit, no overwrite of original evidence |
| Unsafe deletion claim | State-machine deletion, object/database/workflow/cache cleanup, backup-expiry disclosure, periodic deletion verification |

## Authorization rules

- API authentication is necessary but never sufficient; every job, upload, result, review, and correction lookup is tenant-scoped at the repository boundary.
- Product users receive only explicitly granted actions. Service identities have fixed audiences and scopes. Provider workers cannot call review APIs; webhook relays cannot read raw documents.
- Authorization failure and missing object both return 404. Policy-system failure denies access.
- Admin access is time-bound, audited, and cannot bypass residency or retention policy without a separately governed break-glass procedure.

## Data lifecycle

Raw input is quarantined until inspection succeeds. Original, derived pages, result, review correction, and audit records have distinct retention classes. Deletion is idempotent and produces an audit/tombstone record without retaining content. Signed URLs are short lived. Backup retention means deletion is not complete until the documented backup window expires; the API and customer policy must say so.

Kora Langfuse is a downstream observability system, not document storage. Raw OCR content, structured field values, prompts containing document content, and sensitive reviewer corrections are prohibited from its trace attributes and events. Only allowlisted opaque identifiers, operational metadata, bounded error codes, token usage, latency, cost, and model/tool metadata may cross that boundary.

## Security release gates

- Negative tenant tests at upload, job, page, result, cache, review, event, and deletion boundaries.
- Parser fuzzing and hostile corpus tests with resource limits asserted.
- Prompt-injection suite proves untrusted text cannot cause a tool/action/identity/directive escalation.
- SSRF, webhook replay, idempotency, secret redaction, and telemetry-attribute tests.
- Dependency, image, IaC, secret, and license scans; signed artifacts and least-privilege workflow permissions.
- Restore and deletion verification before production readiness.
