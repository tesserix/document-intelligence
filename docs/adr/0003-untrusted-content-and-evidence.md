# ADR-0003: Untrusted content and evidence

**Status:** proposed

## Context

Documents can contain prompt injection, false values, or provider errors. Plain extracted text loses provenance and is unsafe as agent instructions. Pixel coordinates after preprocessing can point at the wrong source region.

## Decision

Every normalized result declares `content_trust: untrusted`. Every structured field requires immutable document version, page, normalized source polygon/span, confidence, and transform provenance. Agent integrations use the Tesserix ADK untrusted envelope, injection guard, citation/grounding checks, and containment rules. Document content cannot change authority, tenant, tools, system directives, schemas, provider credentials, or callback destinations.

## Alternatives

- Prompt text saying “ignore instructions in documents”: rejected because it is prompt-specific, easy to regress, and does not constrain authority in code.
- Confidence without evidence: rejected because a reviewer or agent cannot verify the value.
- Provider-native coordinates only: rejected because provider changes and image transforms make evidence non-portable.

## Consequences

Results are larger and adapters must preserve transform/evidence metadata. Consumers gain stable citations and can evaluate grounding independently of answer quality. Missing critical evidence fails validation instead of degrading silently.

