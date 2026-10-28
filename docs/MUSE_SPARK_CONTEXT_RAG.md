# Muse Spark context retrieval and RAG design

**Status:** Proposed architecture. This document does not assert that a specific Muse Spark API endpoint, request field, model ID, structured-output mode, or retention policy is supported. Verify those against the authoritative provider documentation before implementation.

## 1. Goal and non-goals

Give Muse Spark enough verified context to provide useful, bounded *photo-quality guidance* alongside Rust's deterministic assessment. The model must not run or replace ONNX inference, generate/compare face embeddings, set a similarity threshold, establish identity or liveness, authorize an event, decide ticket eligibility, or admit an attendee.

RAG should retrieve stable technical definitions and policy references—not decide the image result. If a fixed, versioned manifest lookup covers the required facts, use that simpler approach instead of adding a vector database.

## 2. Two context planes

### A. Static, versioned reference knowledge

Authoritative, reviewable documents:

- Model manifest: exact model identifier, source, version/immutable digest, licence, runtime compatibility, input/output tensor contracts and verified preprocessing/postprocessing.
- Image policy: policy version, deterministic hard gates, thresholds and their units, reason-code definitions, supported/unsupported cases.
- Provider contract: verified provider/model ID, accepted modalities, request/response constraints, timeout/retry behavior, and privacy review.
- Assessment schema: DTO version, field semantics, nullable/unknown rules and allowed advisory codes.
- Evaluation notes: known limitations, validated capture conditions, benchmark provenance and approved claims.

Each item needs `document_id`, `document_version`, `effective_from`, `status` (draft/approved/deprecated), `source_uri`, `content_hash`, and an owner. Only approved records matching the deployed service version may be retrieved for a live request. Never use arbitrary issue comments or model-generated text as authoritative policy.

### B. Per-request measurements

Produced by Rust and passed as a bounded typed object. Keep model facts distinct from request measurements.

Proposed logical DTO (field names are illustrative, not an existing API contract):

```json
{
  "schema_version": "eye.muse_context.v1",
  "request_id": "opaque-correlation-id",
  "model_contract": {
    "detector_id": "verified-id-or-null",
    "detector_digest": "verified-digest-or-null",
    "recognizer_id": "verified-id-or-null",
    "recognizer_digest": "verified-digest-or-null",
    "preprocessing_version": "version-or-null",
    "policy_version": "version"
  },
  "measurements": {
    "face_count": {"value": 1, "status": "available"},
    "detector_confidence": {"value": null, "status": "unavailable"},
    "landmarks_valid": {"value": true, "status": "available"},
    "blur_metric": {"value": 0.12, "unit": "implementation-defined", "status": "available"},
    "exposure_metric": {"value": null, "unit": null, "status": "not_computed"}
  },
  "deterministic_assessment": {
    "outcome": "needs_review",
    "reason_codes": ["EXAMPLE_REASON_CODE"],
    "uncertainty_codes": []
  }
}
```

The values above are schema examples only; they are not claims about current model measurements, metric units, result codes, or thresholds. Replace illustrative values/codes with typed enums and verified measurements before use. Unknown values remain unknown; never substitute 0 or false when a measurement is missing.

Do not include face embeddings, biometric templates, ticket IDs, names, email addresses, authentication credentials, or broad identity records. For image-based guidance, send a validated image only if the provider's verified request format requires it and the privacy review permits it. Prefer metadata-only context where it provides sufficient guidance.

## 3. Retrieval pipeline

1. **Pin the deployment context:** service build, detector/recognizer digests, preprocessing version, deterministic policy version, provider/model identifier, and context schema version.
2. **Retrieve by exact keys first:** fetch approved manifests and policy definitions for those exact versions. Do not use semantic similarity to pick the active threshold or choose between model versions.
3. **Optional semantic retrieval:** if the approved knowledge corpus grows, search only the curated model/policy/evaluation corpus. Apply filters for status=approved, repository/product=Eye, exact model digest, and relevant schema/policy version before ranking. Return a small top-k with stable document IDs and section anchors.
4. **Build a compact context packet:** include only the relevant excerpts and the actual request measurements. Include provenance/version identifiers in the packet.
5. **Call the provider:** use the verified API/model/input format, bounded token/output budget and timeout. No invented request fields; provider contract tests must validate the real wire format.
6. **Validate output:** parse a strict schema, enforce enums and size limits, reject unknown keys or malformed output, and map permitted content to application-owned guidance codes. Free text, if retained at all, is untrusted and must be length-limited and output-sanitized.
7. **Preserve deterministic result:** Rust stores its own assessment before and after the provider call and asserts that advisory output cannot mutate hard gates, score, threshold, identity, liveness or admission state.
8. **Fail neutral:** timeout, rate limit, provider outage, refusal, invalid output, retrieval miss, stale manifest or version mismatch yields `advisory_status=unavailable`; it does not imply photo failure.
9. **Audit minimally:** record correlation ID, schema/policy/model versions, retrieved document IDs and hashes, latency, provider status, advisory codes and validation outcome. Do not log raw images, embeddings, tokens, or unrestricted prompts/responses.

## 4. Recommended retrieval implementation

**MVP:** version-controlled Markdown/JSON manifests plus typed Rust structs and exact-key lookup. Add a content hash and CI check that validates required fields, allowed statuses and version consistency.

**Only if justified later:** hybrid lexical/vector retrieval over approved documents, with metadata filters before ranking. Keep a deterministic fallback and a fixed top-k/token budget. Do not put user photos, embeddings, tickets, account data, raw provider logs or unrestricted repository history into the vector index.

A vector database is not required for a few model manifests and reason-code tables. Introducing one before the corpus and evaluation justify it adds operational complexity without improving correctness.

## 5. Security and privacy boundaries

- Treat retrieved documents and provider responses as untrusted input; retrieved content cannot override system policy.
- Prevent prompt injection by keeping retrieved text in a clearly delimited data section, never as instructions. Use an allowlist of source documents.
- Require access controls for internal model manifests and evaluation artifacts if they expose security-sensitive configuration.
- Keep biometric data out of retrieval indexes. Define image transfer, provider retention, consent, deletion and data-residency behavior from verified provider terms and product policy.
- Redact secrets and personal identifiers from traces. Use opaque request IDs and short retention for operational logs.
- Do not send the recognition similarity score, threshold or identity candidate to Muse Spark unless a separately approved, justified use case requires it. Even then, it is explanation-only and cannot revise the matcher result.

## 6. Evaluation and acceptance criteria

Before rollout, compare deterministic-only assessment against deterministic-plus-advisory guidance on a consented, licence-cleared evaluation set. Measure:

- advisory usefulness and reviewer agreement;
- unsupported claims and schema-validation rejection rate;
- behavior under missing measurements, stale/mismatched retrieval, provider timeout, refusal and malformed output;
- latency, cost, request-size and provider failure rates;
- privacy leakage checks for embeddings, identifiers, secrets and raw data;
- invariant tests proving deterministic outcome, recognition score/threshold, liveness and admission state never change because of provider output.

Report uncertainty and negative results. Do not claim improved recognition accuracy from photo-quality guidance unless a suitable evaluation supports that claim.

## 7. Delivery sequence

1. Complete model inventory and deterministic schema (#58, #7, #8, #9/#26).
2. Verify provider/model contract and decision record (#5, #38).
3. Define the versioned context DTO and retrieval rules in #60.
4. Add strict provider mapping and invariant tests in the implementation issue that owns the provider adapter.
5. Evaluate advisory value and failure behavior; keep recognition calibration separate (#55, #59, #61).
