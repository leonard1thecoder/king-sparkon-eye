# Public landing page — design and claim brief

**Issue:** [#172 — Add candidate pricing and original visuals to the public landing page](https://github.com/leonard1thecoder/king-sparkon-eye/issues/172)  \n**CTA polish:** [#174 — Use rounded pill styling for landing-page CTA buttons](https://github.com/leonard1thecoder/king-sparkon-eye/issues/174)  
**Branding:** [#177 — Replace hero artwork with animated King Sparkon Eye branding](https://github.com/leonard1thecoder/king-sparkon-eye/issues/177) (animated hero overridden by [#194](https://github.com/leonard1thecoder/king-sparkon-eye/issues/194)), [#179 — Apply the logo to authentication screens](https://github.com/leonard1thecoder/king-sparkon-eye/issues/179) (merged in [#180](https://github.com/leonard1thecoder/king-sparkon-eye/pull/180))  
**Related design guidance:** [#70 — Prevent generic AI-generated landing page and verify UX quality](https://github.com/leonard1thecoder/king-sparkon-eye/issues/70)  
**Related public-pages scope:** [#35 — Public landing, API, consent, terms and privacy pages](https://github.com/leonard1thecoder/king-sparkon-eye/issues/35)

## Audience and primary action

The page is for event operators, technical evaluators, and integration teams assessing whether King Sparkon Eye fits a QR-free gate workflow. It answers four questions:

1. What does Eye do, and what does it deliberately not decide?
2. How does an event-scoped candidate flow work?
3. Which system owns vision processing, ticket eligibility, and final admission?
4. What is the proposed plan structure and what is actually available today?

Primary actions are to understand the workflow, inspect the canonical OpenAPI contract, and compare the proposed plan tiers. There is no checkout or sales-contact action because billing is not live and no verified sales contact is published.

## Visual direction

Use an editorial product-infrastructure direction rather than a generic SaaS template:

- Black canvas (`--paper: #000`) with light ink, gold accents for the header and CTAs, and a restrained coral focus signal. The page sets `color-scheme: dark`.
- Serif display typography paired with compact monospace system labels.
- A concise product statement paired with the supplied King Sparkon Eye logo in the header, the attendee mobile verification hero image, and a four-step workflow.
- The hero image (`/assets/hero.jpg`) is a static JPEG showing the mobile attendee verification concept at an event entrance. It does not animate, so the hero needs no reduced-motion handling.
- The header shows the supplied logo (`/assets/logo.gif`) with centered navigation links and Sign in (`/auth/sign-in`) and Get started (`/auth/register`) buttons. The footer keeps the compact inline SVG Eye mark, and the shared authentication-page template uses the supplied logo, so the brand remains recognizable without repeating the full wordmark.
- Thin rules, generous whitespace, and purposeful numbered steps instead of decorative metric cards or generic feature-card grids.
- A dark responsibility section to make the ownership boundary memorable.
- A pricing comparison with clear candidate/unapproved status, not an active checkout or implied offer.
- Responsive layouts collapse columns without hiding important information. On narrow screens (700px and below) the text navigation links move into a menu toggle; Sign in and Get started stay visible in the header. The toggle is a checkbox with a label, so the page needs no JavaScript.

The hero image is King Sparkon Eye brand artwork, not a product screenshot, customer image, camera feed, or live system status. The event-scope workflow diagram remains conceptual explanatory artwork. Both raster brand images are embedded in the Eye binary with `include_bytes!` and served from the same origin, so the page adds no external image or CDN dependency. The artwork is brand treatment only; it does not represent a live recognition pipeline.

## Section purpose

| Section | User question answered | Evidence / constraint |
|---|---|---|
| Hero | What is Eye and what is the human-approval boundary? | Issue #35, issue #70, README product goal |
| Gate sequence | What happens from event selection to entry? | README gate verification flow; worker approval is mandatory |
| Responsibility | Which system owns each decision? | AGENTS.md, README architecture |
| Current state | What exists now versus what is planned? | src/api/mod.rs, src/inference/yunet.rs, README implementation boundary |
| Pricing proposal | What plan tiers are being considered? | #137/#138/#145/#154 and docs/BILLING_PLAN_DOMAIN.md; all figures remain unapproved |
| Source links | Where can I inspect the contract and code? | openapi/eye.openapi.json, public GitHub repository |

## Candidate pricing presentation

The public page may display the current planning proposal only when it is unmistakably labelled **unapproved, not live, and not available for purchase**.

| Plan | Monthly base | Annual base | Enrollments/month | Original-photo storage | Active templates | Identifications/month |
|---|---:|---:|---:|---:|---:|---:|
| Free | $0 | $0 | 1,000 | 5 GB | 1,000 | 2,000 |
| Gold | $99 | $990 | 10,000 | 50 GB | 10,000 | 25,000 |
| Premium | $299 | $2,990 | 50,000 | 250 GB | 50,000 | 150,000 |

All figures are candidate USD values, not approved production prices. Proposed paid overages are $0.15 per confirmed enrollment, $0.06 per retained photo GB-month, $0.015 per active template-month, and $0.05 per eligible completed identification. Candidate API limits are Free 60 general requests/minute, 10 inference requests/minute and 2 concurrent inference jobs; Gold 300/60/10; Premium 1,200/300/50. These values require commercial, legal, tax, infrastructure-capacity and Stripe-account validation.

The page must state that the billing catalog is not loaded by the runtime and that checkout, durable usage metering, quota enforcement and subscription management are not live. Annual figures apply to the proposed base subscription; usage is intended to be invoiced monthly in arrears. Do not add a purchase CTA, claim a plan is active, or imply that billing authorizes consent or enrollment.

## Claim controls

- Do not claim liveness detection, identity certainty, accuracy rates, latency, customer adoption, certifications, security guarantees, or production readiness.
- Do not imply ONNX face detection proves identity or liveness.
- Do not imply Rust owns ticket data, eligibility, worker authorization, or admission.
- Do not claim S3 persistence, Kafka delivery, or end-to-end admission integration is live.
- Describe the workflow and SVG scene as explanatory artwork, not a live product UI.
- Keep /api, /consent, /terms, and /privacy as separate deliverables under #35 until implemented and verified. Do not link users to routes that do not exist.
- Pricing and plan limits must remain clearly identified as unapproved proposals until the relevant commercial and implementation gates are cleared.

## Implementation constraints

- Serve a self-contained semantic HTML document from the existing Axum service.
- No frontend framework, external font, image CDN, JavaScript dependency, or new Cargo dependency.
- Keep illustrations self-contained: brand images are embedded with `include_bytes!` and served from the same origin. Do not add remote image requests or unlicensed personal/biometric imagery.
- All navigation targets are real in-page anchors or verified public GitHub resources.
- Use fully rounded pill styling for filled primary CTA buttons and the outlined navigation CTA; keep text links visually distinct. Preserve a visible keyboard-focus outline.
- Respect reduced motion, keyboard focus, mobile breakpoints, semantic landmarks, and descriptive metadata.
- Preserve existing API routes and request middleware.
- Verify the root page's HTML content and responsive/accessibility rules with tests or reproducible browser evidence before declaring the work complete.
