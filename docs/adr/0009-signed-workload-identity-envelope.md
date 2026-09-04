# ADR-0009: signed workload identity envelope

## Context

The document API requires product and tenant scope on every protected request.
Those values cannot originate from browser input, request JSON, object metadata,
or an unverified proxy header. The service runs behind mesh authorization, but
mesh source identity alone does not carry the tenant selected by the product's
already-authenticated user session.

The assets are document contents and tenant-scoped results. Threats include an
unauthenticated caller, a caller from another product, and a compromised
in-cluster workload. The trust boundary is the product API that has verified its
own session before calling the shared OCR service.

## Decision

Each product API receives only its own 32-byte-or-longer signing key from Secret
Manager. It signs a short-lived envelope over key ID, tenant ID, Unix timestamp,
HTTP method, and full path plus query. OCR maps the key ID to the registered
product, validates the HMAC in constant time, validates the tenant identifier,
and permits a maximum 60-second skew by default (configurable only from 1 to
300 seconds). A valid envelope becomes the internal `TrustedIdentity` consumed
by the existing scoped repository boundary.

Protected routes fail closed with `401` when any identity header is missing,
malformed, stale, signed for a different request, or signed by an unknown key.
Only `/healthz` and `/readyz` remain unauthenticated. Key material is redacted
from debug output and never appears in logs, responses, spans, or browser code.

`OCR_WORKLOAD_IDENTITY_KEYS` is supplied only to the OCR workload as
`key_id=product:hex_key` entries. A product receives only the matching key, and
the mesh `AuthorizationPolicy` must allow only that product's service account
to reach OCR. Rotation adds a new key ID, migrates the product signer, then
removes the old key after its maximum envelope lifetime.

## Consequences

The product adapter must sign requests server-side after verifying the user and
must not proxy browser-supplied identity headers. Mutations remain protected by
their existing idempotency key; a captured envelope cannot alter a different
method or URI, and mesh source authorization prevents an external replay path.

The key set is a runtime prerequisite. The executable refuses to start without
it, so a misconfigured deployment fails safely rather than exposing a route that
trusts arbitrary headers. This is a temporary interoperable workload contract;
an OIDC workload-token verifier can replace it only with equivalent issuer,
audience, expiration, key-rotation, and product/tenant binding tests.
