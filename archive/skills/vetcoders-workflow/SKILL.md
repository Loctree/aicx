---
name: vetcoders-workflow
version: 1.0.0
description: >
  This skill should be used when the user asks to "examine and implement",
  "research then implement", "zbadaj i zaimplementuj", "workflow", "pipeline",
  "examine → research → implement", "full workflow", "ERi pipeline", "ERi",
  "plan and implement", "analyze then build", "structured implementation",
  "przebadaj repo i zaimplementuj", or describes a task that requires
  understanding code structure before making changes. Orchestrates a
  three-phase pipeline: Examine (loctree), Research (brave-search/web),
  Implement (subagents). Each phase feeds context to the next.
---

# VetCoders Workflow — ERi Pipeline

**Examine. Research. Implement.**

Three-phase pipeline that chains structural code intelligence, ground truth research,
and parallel agent delegation into a single disciplined workflow.
Each phase accumulates context for the next — no blind implementation.

## Pipeline Overview

```
 EXAMINE (loctree)          RESEARCH (web)           IMPLEMENT (agents)
 ┌─────────────────┐       ┌─────────────────┐      ┌─────────────────┐
 │ repo-view        │       │ brave-search     │      │ write plans      │
 │ focus (1-3 dirs) │──────▸│ WebFetch docs    │─────▸│ spawn agents     │
 │ slice + impact   │       │ Context7 libs    │      │ collect reports  │
 │ find symbols     │       │ curate findings  │      │ review + merge   │
 └─────────────────┘       └─────────────────┘      └─────────────────┘
        ↓                          ↓                         ↓
   CONTEXT.md                 RESEARCH.md              REPORTS/*.md
```

Artifacts accumulate in `.ai-agents/pipeline/<slug>/` per pipeline run.

## Phase 1: EXAMINE

Map the codebase before touching anything. Use loctree MCP tools as the primary layer.

### Required Steps

1. **`repo-view(project)`** — capture health, hubs, languages, LOC
2. **`focus(directory)`** — for each target module (1-3 dirs max)
3. **`slice(file)`** — for every file likely to change (dependencies + consumers)
4. **`impact(file)`** — for high-hub or deletion-candidate files
5. **`find(name)`** — for any new types/functions before creating them
6. **`follow(scope)`** — pursue signals flagged by repo-view (dead, cycles, twins)

### Output: CONTEXT.md

Write structured examination output to `.ai-agents/pipeline/<slug>/CONTEXT.md`:

```markdown
# Examination: <slug>

## Repo Health
- <3-5 bullets from repo-view>

## Scope
- Target dirs: <list>
- Why: <rationale>

## Critical Files
| File | Consumers | Risk | Notes |
|------|-----------|------|-------|

## Symbols Found
- <existing symbols relevant to task>

## Risk Map
- <high-impact files and mitigation strategy>

## Decision
- [ ] Research needed (unknown APIs/patterns)
- [ ] Skip to Implement (well-understood domain)
```

### Phase Gate

Present CONTEXT.md summary to user. Ask: **Research or Implement?**
If domain is well-understood, skip Phase 2. Otherwise proceed.

## Phase 2: RESEARCH

Investigate ground truth — APIs, libraries, prior art, best practices.
Combine brave-search, WebFetch, and Context7 for comprehensive coverage.

### Research Sources (priority order)

1. **Context7** (`resolve-library-id` → `query-docs`) — authoritative library docs
2. **Brave Search** — `python3 ~/.claude/skills/bravesearch/brave_search.py "<query>"`
3. **WebFetch** — fetch specific URLs found via search
4. **Codebase grep** — internal patterns and prior art (only after loctree mapping)

### Query Strategy

Formulate queries from Examination findings:
- Unknown API → `"<API name> usage example <year>"`
- Architecture pattern → `"<pattern> Rust/Swift/etc best practices"`
- Integration → `"<library> + <library> integration"`
- Always append current year for freshness

### Output: RESEARCH.md

Write to `.ai-agents/pipeline/<slug>/RESEARCH.md`:

```markdown
# Research: <slug>

## Questions (from Examination)
1. <question derived from CONTEXT.md>

## Findings
### Q1: <question>
- **Source**: <URL or Context7 lib>
- **Answer**: <concise finding>
- **Code example**: <if applicable>

## Architectural Decision
- Chosen approach: <decision>
- Why: <based on findings>
- Alternatives rejected: <with reasons>

## Implementation Notes
- <concrete guidance for agents>
```

### Phase Gate

Present RESEARCH.md summary. Ask: **Proceed to Implement?**

## Phase 3: IMPLEMENT

Armed with structural context (CONTEXT.md) + research (RESEARCH.md),
delegate implementation to parallel agents.

### Agent Plan Template

Every agent plan MUST include:

1. **Pipeline context** — paste relevant sections from CONTEXT.md + RESEARCH.md
2. **Loctree instruction** — mandatory preamble (proven 98% vs 85% completeness):

```
Use loctree MCP tools as your primary exploration layer:
- repo-view(project) first for overview
- slice(file) before modifying any file
- find(name) before creating new symbols
- impact(file) before deleting
Never edit code without mapping it first.
```

3. **Living tree rule** — standard VetCoders preamble
4. **Quality gate** — repo-specific test/lint commands

### Spawn Pattern

Follow vetcoders-spawn skill for spawn commands (osascript preferred).
Plans go to `.ai-agents/pipeline/<slug>/plans/`.
Reports go to `.ai-agents/pipeline/<slug>/reports/`.

### Review

After agents complete:
1. Read all reports
2. Run quality gate (`make check` or equivalent)
3. Verify against CONTEXT.md risk map
4. Present diff summary to user

## Quick Reference

| Phase | Tool | Output |
|-------|------|--------|
| Examine | loctree MCP | CONTEXT.md |
| Research | brave-search + Context7 + WebFetch | RESEARCH.md |
| Implement | vetcoders-spawn (osascript) | reports/*.md |

## Phase Skipping

Not every task needs all three phases:

- **Small fix, known domain** → Examine only, implement directly
- **New API/library integration** → All three phases
- **Refactor** → Examine + Implement (structure known, no external research)
- **Research only** → Examine + Research (no implementation yet)

State which phases apply at pipeline start.

## Notes

- This skill is mandatory for non-trivial feature work requiring multi-file changes.
- If loctree MCP is unavailable, see `references/phase-examine.md` for grep-based fallback.
- Brave Search path assumes bravesearch skill at `~/.claude/skills/bravesearch/`.

## Anti-Patterns

- Implementing without Examine phase (blind coding)
- Researching without structural context (asking wrong questions)
- Spawning agents without loctree instruction (proven 37% less complete)
- Skipping phase gates (user must approve transitions)
- Not writing CONTEXT.md/RESEARCH.md (context lost between phases)

## Additional Resources

### Reference Files

For detailed phase methodology, consult:
- **`references/phase-examine.md`** — Deep loctree examination patterns
- **`references/phase-research.md`** — Research methodology and source ranking
- **`references/phase-implement.md`** — Agent delegation with accumulated context

### Scripts

- **`scripts/pipeline-init.sh`** — Initialize pipeline directory structure for a new run
