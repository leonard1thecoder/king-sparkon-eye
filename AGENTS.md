# Agent Instructions — King Sparkon Eye

These rules are mandatory for AI coding agents and contributors. For the full product plan and detailed architecture, read the README and the linked issue before starting work.

## Product and integration boundaries

King Sparkon Eye is a standalone, integration-agnostic, multi-tenant identity-verification platform. External applications integrate through Eye's versioned APIs; they retain ownership of their users, customers, employees, tickets, events, deliveries, orders, permissions and business workflows.

Eye owns tenant-scoped identity references, face enrollment/templates, face-only inference, 1:1 matching, explicitly scoped tenant-local 1:N identification, verification sessions, results and the platform controls assigned to it by approved contracts.

Non-negotiable invariants:

- Identity data, templates, scopes, sessions, results, evidence, credentials and audit access are isolated by tenant. Never implement cross-tenant lookup, template reuse, candidate search or global face search.
- Verification is face-only in the current product scope. Do not add OTP or document verification without an explicit product decision.
- Asynchronous processing is the target default. Do not describe synchronous photo assessment as an asynchronous verification session.
- 1:1 `match` compares against one specified tenant-owned identity. 1:N `identify` searches only an explicit tenant-owned identity scope.
- A face similarity result is probabilistic. It is not proof of legal identity, liveness, consent or business authorization.
- The integrating application owns business entities, permissions, workflow state, consent capture and the final business decision.
- Eye must not own ticket management, gate admission, event operations or another application's business workflow.
- Never trust a caller-supplied tenant identifier as authorization. Resolve tenant context from validated credentials.
- Do not invent model capabilities, API endpoints, schemas, database columns, event names or security guarantees. Verify repository source, tests and approved contracts; label proposals as proposed.

Tracker repositories are **not mandatory source-of-truth dependencies** for ordinary Eye work. Inspect another repository only when a separately approved integration issue requires compatibility with that specific provider or consumer. Such cross-repository work must have its own linked issue, branch and PR in each affected repository. Do not infer private contracts or change another repository as a side effect of an Eye issue.

## One issue = one branch = one PR

For every actionable issue that changes repository contents (code, tests, docs, config, schema, workflow):

1. Refresh main and create a dedicated branch from it named issue/<issue-number>-<short-slug>.
2. Work only on that issue's scope on that branch. Do not bundle separate issues.
3. Open one PR in the same repository for the issue. Include the issue number in the title and Closes #<number> in the body for a same-repository issue.
4. Never commit directly to main and never reuse one branch for multiple issues.
5. Epics are tracking-only. Deliver their work through individual task issues, each with its own branch and PR.
6. For a change spanning Eye and either tracker repo, create a separate issue, branch and PR in each affected repository. Cross-link the related issues and PRs.
7. Do not create an empty PR merely to satisfy policy. If an issue changes repository content, it must use this workflow; pure tracking-only epics do not need implementation PRs.

Branch example: issue/31-private-s3-storage.

## Mandatory bug-fix workflow

Before changing code to fix a bug, every agent MUST complete these steps:

1. **Investigate and search first.** Reproduce or inspect the reported failure where possible. Search this repository's existing issues, including open and closed issues, using error messages, affected components, symptoms and likely root-cause terms. Search related PRs and inspect the relevant source, tests, configuration and recent changes. Do not rely on title-only matching; check issue descriptions and comments when available.
2. **Reuse an existing issue when it matches.** If a matching bug issue exists, use it as the work item. Add verified reproduction details, impact, scope and testable acceptance criteria if missing. Cross-link related issues rather than creating duplicates.
3. **Create an issue if none matches.** Before implementing an untracked bug fix, create a dedicated bug issue. Include:
   - concise symptom-focused title;
   - observed behavior and expected behavior;
   - reproduction steps, logs or evidence available (redact secrets and personal data);
   - impact/severity and affected component, distinguishing confirmed facts from hypotheses;
   - testable acceptance criteria, including regression coverage where practical.
   Do not fabricate reproduction steps, logs, impact, root causes or evidence. If the failure cannot yet be reproduced, state that clearly and make investigation part of the issue.
4. **Create a dedicated issue branch.** Start from the current default branch and use issue/<issue-number>-<short-slug>. Do not begin implementation on main, an unrelated branch or a branch shared with another issue.
5. **Fix only the scoped bug.** Add or update regression tests where practical. Keep unrelated refactors and enhancements out of the bug branch; create separate linked issues for follow-up work.
6. **Verify before opening the PR.** Run the relevant tests, formatting, lint, contract checks and build checks available for the affected code. Report exact commands and results, including checks that could not run and why. Never claim a check passed if it was not run.
7. **Open a linked PR.** Submit one PR from the issue branch to the default branch. Include the issue number in the title and use Closes #<number> in the PR body when the PR fully resolves the same-repository issue. Summarize root cause (only when verified), fix, regression tests, validation results and residual risks.
8. **Close only after verification and merge.** Do not close the bug issue merely because a fix was committed or a PR was opened. Close it after the fix is merged and the acceptance criteria are met, following repository policy.

If an urgent production mitigation must happen before a normal issue can be created, document the reason and create/link the issue as soon as practical; do not use urgency as a general bypass for review.

## Required workflow before implementation

1. Read this file, README, the issue, related parent/task issues, and relevant recent PRs.
2. Inspect Eye source code, tests, contracts, migrations and configuration. Inspect another repository only when the issue explicitly requires a separately approved integration.
3. Add current-state evidence to the issue/PR: what files/contracts were inspected and what compatibility must be preserved.
4. Implement the smallest reviewable change and add/update tests.
5. If implementation reveals a conflict with the intended architecture, stop and document the evidence/trade-offs; update the parent issue/README before changing architecture.
6. Update docs/contracts/issues in the same issue branch/PR whenever observable behavior changes.
7. Run relevant format, lint, test, contract and build checks before opening/updating the PR.
8. Report changed files, commands run, results, risks and known limitations. Close the issue only when acceptance criteria pass.

## Cross-repository boundary

- Changes to current UI/auth/API client patterns belong in king-sparkon-tracker.
- Changes to ticket/event/user/worker business rules, database schema, ticket status and admission authorization belong in king-sparkon-tracker-backend.
- Rust vision, ONNX/model inference, photo validation and recognition-result production belong in king-sparkon-eye.
- Changes requiring more than one repository must be split into linked repository-specific issues/PRs. Do not guess a contract when the consumer/provider repo change is not yet merged or explicitly agreed.

## Mandatory issue-linked documentation synchronization

Whenever an issue is fixed, implemented, or its status/acceptance criteria materially change, update **all Markdown documents affected by that issue in the same issue branch and PR**. This is a required acceptance criterion, not optional cleanup.

1. Identify impacted Markdown by searching the repository for the issue number, feature name, API/event/schema/config terms, and user-visible behavior. Inspect the README, relevant `AGENTS.md`/playbook guidance, architecture/API/decision docs, and affected feature docs as applicable.
2. Update every document whose claims, status, examples, instructions, contracts, prerequisites, or roadmap are changed by the fix. At minimum, check README and the implementation playbook when the change affects implementation status, execution order, agent workflow, or architecture.
3. Do not blindly edit unrelated Markdown. If a document was inspected and does not need a change, record why in the PR summary or checklist.
4. Keep implemented, partial, planned, blocked, and unverified states distinct. Never describe a PR as completed before it is merged, or a feature as shipped without code/test evidence.
5. Synchronize linked issue acceptance criteria/checklists and related docs when they have become stale. Cross-link follow-up work rather than silently broadening the current issue.
6. In the PR body, list all Markdown files reviewed and changed, and state any relevant files intentionally left unchanged with a short reason. Validate links, headings, code fences, and consistency between README, `AGENTS.md`, playbook, and contracts.
7. A fix is not documentation-complete until the issue's relevant Markdown has been reconciled and the handoff says what changed, what did not change, and what remains blocked.

## Documentation accuracy

Distinguish implemented, partial, planned and unverified functionality. Code and tests establish what is currently implemented; issue acceptance criteria establish what the task must add. Keep README, API schemas, Kafka event schemas and issue checklists synchronized.



## Mandatory issue-scoped documentation synchronization

Every issue fix includes reviewing and updating the documentation affected by that issue. This is part of the fix, not optional cleanup.

1. **Discover affected Markdown.** Search and inspect the README, this file, `docs/IMPLEMENTATION-PLAYBOOK.md`, relevant architecture/design notes, API or model-contract docs, test/development instructions, operations/privacy docs, and any other Markdown describing the affected behavior. Follow links from the issue, related issues and relevant PRs.
2. **Update all relevant docs in the same issue branch and PR.** Make each affected Markdown file accurate for the verified implementation and tests. Update README feature/status claims, contracts, setup/test commands, limitations and examples when the issue changes them. Do not change unrelated documents just to increase the file count.
3. **Keep the playbook current.** Update `docs/IMPLEMENTATION-PLAYBOOK.md` when work changes implementation status, issue sequencing/dependencies, blockers, ownership boundaries, validation requirements or agent workflow. Refresh dated status snapshots from current issue/PR/source evidence; preserve historical snapshots as historical rather than silently rewriting them.
4. **Keep agent rules aligned.** Update `AGENTS.md` when a fix exposes a reusable workflow rule or changes a mandatory constraint. Ensure it does not conflict with the playbook, README or security/privacy invariants.
5. **Synchronize issue and PR evidence.** Update acceptance criteria/checklists and cross-link related issues when the scope or verified status changes. In the PR description, list Markdown files reviewed, files updated, and the reason for leaving any materially relevant file unchanged.
6. **Verify the documentation.** Check links and paths, terminology, contract examples, implementation-status claims, dependency order and consistency across affected Markdown. Run applicable documentation/link checks when available; report exact commands and results. Never claim a check ran when it did not.
7. **Treat docs as part of done.** Do not mark the issue complete while relevant documentation is stale or contradictory. If a needed decision is unresolved, document the known facts, unknowns and blocker without inventing behavior.

This requirement applies to bug fixes, features, refactors, configuration/schema changes and documentation issues. The scope is every *relevant* Markdown file, not every Markdown file in the repository.

## Evidence-first issue triage and delivery

- At task start, refresh the default branch, inspect the exact issue body and comments, and search open/closed issues and PRs for overlap. An issue title or priority prefix is not proof that its prerequisites are satisfied.
- Treat P0/P1/P2 as proposed sequencing signals, not permission to skip dependencies, security review, model validation, or acceptance criteria. Record blockers and dependency edges explicitly.
- Separate tracking epics, architecture decisions, documentation tasks, implementation tasks, and evaluation gates. Do not treat an epic as an implementation ticket or mark an issue complete based on a PR being opened.
- For audits, report the issue number, current title, proposed priority, dependency/blocker, and a concrete next action. Distinguish observed repository facts from recommendations.
- Before starting an implementation issue, confirm its prerequisites are merged or explicitly document why a safe independent slice is possible. Avoid parallel work that changes the same contract without an agreed owner.

## Muse Spark and retrieval/context policy

- Muse Spark is a supplementary visual-quality explainer/advisor only. It is not the face detector, embedding generator, identity matcher, liveness authority, consent authority, ticket eligibility service, or admission decision-maker.
- Build provider context from versioned, typed Rust-owned evidence. Include only fields actually produced by the active pipeline; unavailable values remain `null`/unknown with an explicit availability status, never fabricated defaults.
- Keep model-contract knowledge (model ID/version or artifact digest, input/output contract, preprocessing, threshold/policy versions) separate from per-request measurements (face count, detector confidence, landmark validity, image-quality metrics, deterministic assessment and reason codes).
- If retrieval is used, retrieve only approved, versioned model manifests, policy docs, and reason-code definitions. Apply repository/version filters and exact model/policy-version matching. Do not retrieve arbitrary user content as authoritative policy.
- Do not send face embeddings, biometric templates, ticket identifiers, authentication tokens, raw identity records, or unrelated personal data to Muse Spark by default. Minimize image transmission; verify provider request format, data handling, and model identifier against authoritative provider documentation before implementation.
- Treat all provider output as untrusted advice. Validate against a strict schema, bound its size, map it to application-owned advisory codes, and keep it out of deterministic decision fields. Provider timeout, malformed output, refusal, or outage must not turn a passing image into a failing image or vice versa.
- Keep prompt, retrieved-document IDs/versions, provider/model version, input-schema version, and advisory result code in privacy-safe audit metadata. Never log raw photos, embeddings, secrets, or unrestricted provider payloads.
- RAG is not required for a small, fixed set of stable model facts. Prefer typed configuration/manifests and direct lookup for deterministic runtime behavior; add semantic retrieval only when the corpus and retrieval evaluation justify it. Retrieval must never select or change a recognition threshold at runtime.

## Face-recognition evidence and safety gates

- Do not document an ONNX model's identity, version, dimensions, normalization, alignment, outputs, embedding dimension, metric, or threshold until verified against the actual artifact metadata, authoritative model documentation, and Rust preprocessing/postprocessing.
- Detector confidence, landmark validity, photo quality, embedding similarity, liveness, and admission are separate concepts and must remain separate in APIs, logs, tests, and UI.
- Recognition threshold changes require versioned evaluation evidence on licensed/consented, subject-disjoint data. Synthetic tensors alone do not establish real-world recognition accuracy.
- A candidate match remains a candidate. Ambiguous or out-of-distribution cases must abstain or route to manual review; no Rust result or Kafka delivery can automatically admit a ticket.
