# OCR MCP access grants

`document-intelligence-mcp` is a stateless, read-only MCP server for an
already-authorised product backend.  It does not accept a tenant or subject as
a tool argument.  The backend derives both values from its verified product
session, then supplies a short-lived, signed access grant for each tool call.

This is separate from the gateway's `X-MCP-Key`: AgentGateway authenticates the
route and injects that key, while this grant proves which product, tenant, and
subject are authorised to read the exact requested OCR object.

## Product signer contract

The product backend sends these headers on `tools/call`:

| Header | Value |
| --- | --- |
| `X-OCR-Key-Id` | configured key identifier |
| `X-OCR-Tenant-Id` | tenant derived from the verified product session |
| `X-OCR-Subject` | authenticated caller subject |
| `X-OCR-Timestamp` | Unix timestamp in seconds |
| `X-OCR-Grant-Signature` | lowercase hexadecimal HMAC-SHA256 |

The signature input is exactly six newline-separated fields:

```text
key_id
tenant_id
subject
unix_timestamp
tool_name
sha256:<lowercase hex digest of canonical JSON tool arguments>
```

Canonical JSON preserves scalar encoding and array order, recursively sorts
object keys lexicographically, and emits no insignificant whitespace.  The
grant must be regenerated for every tool call; the default maximum clock skew
is 60 seconds.  It binds the signature to both the exact allowlisted tool name
and its arguments, so a grant for one job cannot be replayed for another.

The only version-one tools are `get_document_status` and
`get_document_result`, both requiring `{ "job_id": "..." }`.  Results are
returned as untrusted data and must never be interpreted as instructions.

## Runtime configuration and rotation

The MCP workload receives no product-side signing credential.  It receives a
verifier keyring through `OCR_MCP_ACCESS_GRANT_KEYS`:

```text
key_id=product:hex_encoded_32_byte_or_longer_key[,next_key_id=product:hex_key]
```

`OCR_MCP_UPSTREAM_KEYS` maps each product's distinct AgentGateway-injected
credential in the form `product=hex_encoded_32_byte_or_longer_key`.  Both
variables are secret references only; never commit their values, expose them to
an MCP client, or reuse a credential from another MCP server.

Rotate by adding a new key ID to the verifier keyring, migrating the matching
product signer, validating a harmless signed read, waiting longer than the
clock-skew window, and then removing the retired key ID.  A missing, malformed,
expired, wrong-product, or incorrectly scoped grant fails closed.

## Product onboarding gate

Before publishing a product route, provision a dedicated product signer,
dedicated upstream MCP key, result-reader workload identity, product-scoped
OCR result bucket, and an approved smoke-test document.  Do not create a
Registry record or AgentGateway route until all prerequisites are ready: a
published route with no signer or result scope is intentionally unavailable.

See the [product MCP onboarding guide](https://github.com/tesserix/australis/blob/main/docs/guides/product-mcp-onboarding.md)
for the platform ownership and registry requirements, and
[PRODUCT-OCR-INTEGRATION.md](PRODUCT-OCR-INTEGRATION.md) for the direct OCR API
boundary.
