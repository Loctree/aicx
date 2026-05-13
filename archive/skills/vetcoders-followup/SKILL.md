---
name: vetcoders-followup
version: 1.0.0
description: >
  Production follow-up audit skill for VetCoders. Runs a strict chain:
  vetcoders-init -> vetcoders-workflow -> vetcoders-spawn, then synthesizes
  implementation gaps, readiness risks, quality gate outcomes, and next actions
  before hands-on build/testing.
---

# vetcoders-followup

Follow-up skill for post-implementation review of agent-delivered work.
Use this when the team asks for "follow-up check", "czy sa jeszcze luki",
"readiness before hands-on", or "audit this implementation after plan execution".

## Core Contract

Always run this sequence:
1. `vetcoders-init` (memory + structure baseline)
2. `vetcoders-workflow` (Examine -> optional Research -> Implement context)
3. `vetcoders-spawn` or `vetcoders-implement` (parallel delegated audits)
4. Final synthesis by primary agent (you)

Do not skip sequence unless user explicitly opts out.

## Required Inputs

- Repo root path (`$ROOT`)
- Pipeline slug (for example `YYYYMMDD_followup-agent2`)
- Target scope (modules/features touched today)
- Quality gate commands for this repo

## Non-Negotiables

- Use loctree MCP as first exploration layer; fail-fast if snapshot is stale.
- Use semgrep as first security gate.
- Rust repos: run `cargo clippy -- -D warnings`.
- Run tests relevant to touched surfaces plus at least one cross-pipeline e2e test.
- Findings are primary output, sorted by severity (`P0`, `P1`, `P2`).
- Every finding must include concrete file references.

## Phase A - Bootstrap (vetcoders-init)

1. Extract context memory:
```bash
aicx all -p <project> --incremental
aicx refs -H 168 -p <project>
```
2. Map structure:
- `repo-view(project)`
- `focus(directory)` for 1-3 target dirs
- `follow(scope="all")` when risk signals appear
3. Produce situational summary with:
- open signals from memory
- repo health and top hubs
- target scope and risks

## Phase B - Workflow Context (vetcoders-workflow)

Create pipeline artifacts:
- `.ai-agents/pipeline/<slug>/CONTEXT.md`
- `.ai-agents/pipeline/<slug>/RESEARCH.md`
- `.ai-agents/pipeline/<slug>/plans/`
- `.ai-agents/pipeline/<slug>/reports/`

`CONTEXT.md` must include:
- repo health
- critical files table
- risk map
- phase decision (research needed or not)

`RESEARCH.md` should be concise and only cover unknowns that affect architecture,
runtime behavior, or production readiness.

## Phase C - Delegation (vetcoders-spawn)

Prepare at least 2 audit tracks:
1. Runtime/architecture audit
2. Hands-on readiness and gate audit

### Mandatory preamble in each plan

```text
You work on a living tree with Vibecrafting methodology, so concurrent changes are expected.
Adapt proactively and continue, but this is never permission to skip quality, security, or test gates.
Run required checks. If something is blocked, report the exact blocker and run the closest safe equivalent.
```

### Subagent plan skeleton

```markdown
# Task: <short title>

Goal:
- <1-3 bullets>

Scope:
- In scope: <areas>
- Out of scope: <areas>

Acceptance:
- [ ] <objective outcome>
- [ ] <objective outcome>
- [x] refinement

Test gate:
- semgrep --config auto --error --quiet
- cargo clippy -- -D warnings
- <repo specific tests>

Context:
- <short context from CONTEXT.md + RESEARCH.md>
```

### Spawn method

Use canonical Terminal spawn (`osascript`) from `vetcoders-spawn`.
Write plans to `.ai-agents/.../plans/` and reports to `.ai-agents/.../reports/`.

## Phase D - Follow-up Synthesis (Primary Agent)

After reports arrive:
1. Validate subagent claims against real code (`slice/find/rg` + file reads).
2. Re-run mandatory gates on current tree.
3. Build one consolidated verdict:
- implementation gaps
- blockers vs caveats
- readiness score
- exact next fixes

## Severity Model

- `P0` - release/blocking issue (crash, compile break, wrong runtime path)
- `P1` - high risk / likely regression in production flows
- `P2` - quality/usability/observability gap, non-blocking

## Output Format (Mandatory)

Use this exact top-level structure in final response:

```text
Current state: <what is wrong or still risky>
Proposal: <target state + why>
Migration plan: <ordered concrete steps>
Quick win: <one immediate high-impact action>
```

Then include:
- Findings list (P0/P1/P2, with file refs)
- Gate results (pass/fail + command)
- Hands-on readiness verdict (`GO`, `GO-WITH-CAVEATS`, `NO-GO`)
- Confidence score (0-100)

## Ready-to-Use Prompts

### Prompt 1 - Runtime audit subagent

```text
Task: Follow-up runtime audit for Agent implementation

Review runtime routing, tool execution path, fallback behavior, and persistence consistency.
Focus on whether new runtime is truly used in all intended paths.

Deliver:
- P0/P1/P2 findings with file:line evidence
- Exact mismatch points between intended and actual runtime
- Minimal remediation sequence

Run gates:
- semgrep --config auto --error --quiet
- cargo clippy -- -D warnings
```

### Prompt 2 - Hands-on readiness subagent

```text
Task: Hands-on readiness follow-up audit

Verify that codebase is ready for manual desktop testing:
- build/test gates
- operational assumptions (env/permissions)
- UX blockers for real-world session

Deliver:
- blocker/caveat list
- readiness verdict and confidence score
- concrete smoke checklist for desktop test session

Run gates:
- semgrep --config auto --error --quiet
- cargo clippy -- -D warnings
- targeted tests for touched modules
```

## Anti-Patterns

- Treating green unit tests as production readiness proof.
- Reporting only summary without severity-ranked findings.
- Skipping file-level evidence.
- Running subagents without living-tree preamble.
- Ending without a clear `GO/NO-GO` recommendation.

---

VetCoders follow-up principle:
"Green gates are necessary, not sufficient. Runtime truth wins."
