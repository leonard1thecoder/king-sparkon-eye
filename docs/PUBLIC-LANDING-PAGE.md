# Public landing page — design and claim brief

**Issue:** [#70 — Prevent generic AI-generated landing page and verify UX quality](https://github.com/leonard1thecoder/king-sparkon-eye/issues/70)  
**Related scope:** [#35 — Public landing, API, consent, terms and privacy pages](https://github.com/leonard1thecoder/king-sparkon-eye/issues/35)

## Audience and primary question

The page is for event operators, technical evaluators, and integration teams assessing whether King Sparkon Eye fits a QR-free gate workflow. It answers three questions: what Eye does, how an event-scoped candidate flow works, and who makes the admission decision.

## Visual direction

Use an editorial product-infrastructure direction rather than a generic SaaS hero:

- Warm paper canvas, deep forest ink, restrained coral signal, and a small acid-green accent.
- Serif display typography paired with compact monospace system labels.
- A clear product statement paired with a four-step workflow schematic.
- Thin rules, generous whitespace, and purposeful numbered steps instead of decorative metric cards or feature-card grids.
- A dark responsibility section to make the ownership boundary memorable.
- Responsive layout collapses workflow and responsibility columns without hiding information.

The workflow schematic is explicitly labelled illustrative. It is not a screenshot, live status panel, or claim that the full flow is deployed.

## Section purpose

| Section | User question answered | Evidence / constraint |
|---|---|---|
| Hero | What is Eye and what is the safety boundary? | Issue #35, issue #70, README product goal |
| Gate sequence | What happens from event selection to entry? | README gate verification flow; worker approval is mandatory |
| Responsibility | Which system owns which decision? | AGENTS.md, README architecture |
| Current state | What exists now versus what is planned? | src/api/mod.rs, src/inference/yunet.rs, README implementation boundary |
| Source links | Where can I inspect the contract and code? | openapi/eye.openapi.json, public GitHub repository |

## Claim controls

- Do not claim liveness detection, identity certainty, accuracy rates, latency, customer adoption, certifications, or production readiness.
- Do not imply ONNX face detection proves identity or liveness.
- Do not imply Rust owns ticket data, eligibility, worker authorization, or admission.
- Do not claim S3 persistence, Kafka delivery, or end-to-end admission integration is live.
- Describe the diagram as an explanatory workflow, not a live product UI.
- Keep /api, /consent, /terms, and /privacy as separate planned deliverables under #35 until implemented and verified. Do not link users to routes that do not exist.

## Implementation constraints

- Serve a self-contained semantic HTML document from the existing Axum service.
- No frontend framework, external font, image CDN, JavaScript dependency, or new Cargo dependency.
- All navigation targets are real in-page anchors or verified public GitHub resources.
- Respect reduced motion, keyboard focus, mobile breakpoints, semantic landmarks, and descriptive metadata.
