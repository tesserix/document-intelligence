# ADR-0006: Kora runtime storage as a consumer integration

**Status:** proposed

## Context

Document Intelligence is a reusable service for multiple products and agents.
Kora documents can contain sensitive personal, health or financial data and need
product-specific retention and access. Shared OCR evaluation/training assets must
not be named, owned or credentialed as Kora.

## Decision

Treat Kora as one consumer. Use Kora-specific runtime buckets for quarantine,
accepted source, temporary derived pages and normalized results. Isolate objects
by opaque tenant/document/version paths, immutable generations, generation
preconditions and object authorization. Kora calls versioned service endpoints or
the Australis tool and receives no storage/database credentials.

Keep shared sandbox, dataset candidates, train/calibration, development
evaluation, held-out golden, evaluation results and candidate models under
product-neutral Document Intelligence identities and manifests. Kora runtime and
Langfuse credentials cannot access those stores; shared training/evaluation
identities cannot browse Kora production documents.

## Alternatives

- Kora owns OCR evaluation/training: rejected because it couples a reusable
  service to one product and risks cross-product policy/data leakage.
- One bucket per document: rejected because bucket quota, IAM and lifecycle
  overhead do not replace object-level authorization.
- One bucket for all Kora stages: rejected because untrusted intake, temporary
  transforms and authoritative results need different identities and retention.
- Give Kora agents direct GCS access: rejected because it bypasses service
  authorization, audit, evidence and deletion contracts.

## Consequences

Kora receives strong product/data-class isolation while the OCR engine, APIs,
models and evaluations remain reusable. This requires separate product runtime
and shared evaluation/training GitOps resources plus explicit mutual-deny tests.
Physical names, regions, identities, encryption and retention remain review gates.
