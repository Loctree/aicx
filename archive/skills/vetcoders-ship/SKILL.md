---
name: vetcoders-ship
version: 1.0.0
description: >
  Master orchestrator for the VetCoders pipeline. One command runs the
  full Build → Converge → Ship sequence automatically. The user says
  "ship it" and the pipeline handles the rest: bootstrap context, run
  the ERi workflow, audit findings, loop marbles until P0=P1=P2=0,
  measure the product surface, and hydrate for market.
  All sub-skills remain individually callable — ship just chains them.
  Trigger phrases: "ship", "ship it", "full pipeline", "run everything",
  "od zera do launchu", "caly pipeline", "wypusc to", "from scratch to
  market", "end-to-end", "push to market", "zrob wszystko".
---

# vetcoders-ship — The Full Pipeline in One Command

> One command. Three phases. Circle full. Product shipped.

## What This Does

vetcoders-ship is the master orchestrator. It chains the entire VetCoders
pipeline into a single automated sequence:

```
Phase 1 — Build:     init → workflow → followup
                                         ↓
Phase 2 — Converge:                  marbles ↻ (loop until P0=P1=P2=0)
                                         ↓
Phase 3 — Ship:                      dou → hydrate
```

The user provides a task description. Ship does the rest.

implement/spawn are the execution engines used internally by workflow
and marbles — they are not pipeline steps the user invokes.

## When To Use

- Starting a new feature or task from scratch
- The user says "ship it" or "full pipeline" or "run everything"
- When the user wants the complete Build → Converge → Ship cycle
- When the team wants end-to-end automation without manual phase transitions

When NOT to use:
- The user explicitly wants just one phase (use the individual skill)
- Mid-pipeline recovery (pick up from the specific skill that failed)
- Quick exploration with no intent to ship

## The Sequence

### Phase 1: Build

Run these three skills in order. Each one feeds the next.

#### Step 1 — init

Read and execute `vetcoders-init/SKILL.md`.

This bootstraps context: extracts session history via ai-contexters,
maps repo structure via loctree MCP, produces a situational report.

The output is the foundation for everything that follows. If init fails
(missing tools, no repo context), stop and tell the user what's missing.

Gate: init must produce a situational report before proceeding.

#### Step 2 — workflow

Read and execute `vetcoders-workflow/SKILL.md`.

This runs the ERi pipeline: Examine (loctree deep scan) → Research
(web/docs if needed) → Implement (delegate to subagents via
implement or spawn).

The task description the user provided goes here. Workflow takes the
context from init and turns it into code.

Gate: workflow must complete implementation before proceeding.

#### Step 3 — followup

Read and execute `vetcoders-followup/SKILL.md`.

This audits what workflow built. Quality gates, security scan,
completeness check. Produces a P0/P1/P2 findings list and a
GO / NO-GO verdict.

Gate: followup must produce a findings report. The verdict does NOT
need to be GO to proceed — that's what Phase 2 is for.

### Phase 2: Converge

#### Step 4 — marbles

Read and execute `vetcoders-marbles/SKILL.md`.

This is the iterative convergence loop. Marbles takes the findings
from followup and loops: fix → measure → repeat. Each loop throws
marbles at the known gaps until the circle is full.

The loop runs until P0=0, P1=0, P2=0. There is no fixed iteration
count. Measurement drives the schedule.

Gate: marbles must reach convergence (P0=P1=P2=0) or the user
must explicitly accept remaining risk.

### Phase 3: Ship

#### Step 5 — dou

Read and execute `vetcoders-dou/SKILL.md`.

Definition of Undone audit across the entire product surface.
Crawls URLs, checks repo governance, install paths, discoverability,
monetization. Produces Undone Matrix and Plague Score.

Gate: dou must produce an Undone Matrix before proceeding.

#### Step 6 — hydrate

Read and execute `vetcoders-hydrate/SKILL.md`.

Packages the product for market. Fixes what dou found: repo governance,
SEO, distribution channels, marketplace listings, onboarding flows.
The antidote to Always-in-Production.

Gate: hydrate must produce "Done Done" artifacts.

## Phase Transitions

Each phase transition is a checkpoint. Report progress to the user:

```
Phase 1 complete.
  init: situational report written
  workflow: implementation complete (N files, M LOC)
  followup: P0=X, P1=Y, P2=Z — verdict: <GO/NO-GO>
Entering Phase 2 (Converge)...
```

```
Phase 2 complete.
  marbles: converged in N loops
  final: P0=0, P1=0, P2=0 — score: 100
Entering Phase 3 (Ship)...
```

```
Phase 3 complete.
  dou: Plague Score = X → Y
  hydrate: N artifacts generated
Pipeline complete.
```

## Error Handling

If any skill fails:

1. Report which skill failed and why
2. Show what was accomplished so far
3. Suggest the specific skill to re-run manually
4. Do NOT silently skip failed steps

If marbles diverges (entropy increases instead of decreasing):

1. Stop the loop
2. Report the divergence to the user
3. Suggest re-running workflow on the affected area
4. Do NOT continue blind iteration

## Output

Ship produces a final summary after all phases complete:

```markdown
# Ship Report: <task>
Date: <YYYY-MM-DD>

## Phase 1 — Build
- init: <status>
- workflow: <files changed, LOC delta>
- followup: <initial P0/P1/P2 counts>

## Phase 2 — Converge
- marbles loops: N
- trajectory: <score progression>
- final: P0=0, P1=0, P2=0

## Phase 3 — Ship
- dou Plague Score: before → after
- hydrate artifacts: <list>

## Result
Pipeline: COMPLETE
The circle is full. The product ships.
```

Save the report to `.ai-agents/pipeline/<slug>/ship-report.md`.

## Individual Skill Access

Every sub-skill remains independently callable. Ship is a convenience
wrapper, not a replacement. If the user wants to run just marbles,
or just dou, or just workflow — they can. Ship just chains them.

This means:
- `/vetcoders-init` still works on its own
- `/vetcoders-workflow` still works on its own
- `/vetcoders-followup` still works on its own
- `/vetcoders-marbles` still works on its own
- `/vetcoders-dou` still works on its own
- `/vetcoders-hydrate` still works on its own

Ship adds orchestration. It does not add lock-in.

## Anti-Patterns

- Skipping phases ("just ship it without converging") — defeats the purpose
- Running ship on trivial changes (single-line fix) — overkill, use workflow directly
- Ignoring phase transition reports — they exist for a reason
- Re-running ship instead of picking up from a failed step — wasteful

## The Philosophy

The pipeline exists because AI agents solve the hard part (making it work)
but not the second-hard part (making it reachable). Ship closes that gap
in one automated sequence.

Phase 1 builds it right.
Phase 2 fills the circle.
Phase 3 puts it in someone's hands.

---

*"One command. Three phases. Circle full. Product shipped."*

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
