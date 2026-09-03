# ADR-0001: Service and repository boundaries

**Status:** proposed

## Context

Document processing has a distinct hostile-input boundary, scaling profile, provider lifecycle, retention policy, and accuracy evaluation. Agent reasoning and review UI have different owners and release risks.

## Decision

`document-intelligence` owns intake through normalized, validated, cited results and review decisions. `ai-agents` owns reasoning. Australis owns the provider-neutral agent tool and grounding integration. DevAI owns sandbox/evaluation/promotion. A review application consumes the review API but is not embedded here.

## Alternatives

- Put OCR in every product: rejected because security, retries, evidence, evaluation, and provider coupling would drift.
- Put OCR inside Australis: rejected because non-agent product callers also need it and provider/parser failures should not share the assistant engine's release boundary.
- Put the document agent inside the OCR service: rejected because untrusted data would sit beside model authority and service quality could not be evaluated separately from reasoning.

## Consequences

The public contracts outlive implementations and require compatibility testing. Cross-repository promotion must prove a compatible service/tool/agent set. The additional network boundary is accepted for isolation and reuse.

