---
name: vetcoders-init
version: "1.0"
description: >-
  This skill should be used when the user asks to "init", "initialize session",
  "give context to agent", "prepare agent", "bootstrap agent", "daj kontekst",
  "zainicjuj", "przygotuj agenta", "init session", "start fresh with context",
  or when starting work on a repo and the agent needs situational awareness.
  Combines aicx (session history extraction) with loctree MCP
  (codebase structure mapping) to equip the agent with memory and visibility
  before implementation work.
---

# vetcoders-init — Memory + Eyes for AI Agents

Bootstrap an agent session with two layers of context:
- **Memory**: What was done before (ai-contexters — session history extraction)
- **Eyes**: What the code looks like now (loctree MCP — structural map)

## When To Use

Execute at the start of every session, **before any implementation work**:
- **Cold start**: First session on a repo (zero prior context)
- **Resuming after break**: Stale context after 24+ hours away
- **Subagent delegation**: Agents inherit structured context
- **Structural drift**: Major changes by others since last session

Running init is a forcing function: it prevents blind coding.

## Init Sequence

### Step 1: Extract Memory

Pull historical context from previous AI sessions for this project:

```bash
aicx all -p <project_name> --incremental
```

This extracts deduplicated, chunked timelines from Claude Code, Codex, and
Gemini sessions into `~/.ai-contexters/<project>/`. Incremental mode skips
already-processed entries.

**If no project name is obvious**, detect from repo:
```bash
PROJECT=$(basename "$(git rev-parse --show-toplevel)")
aicx all -p "$PROJECT" -H 168 --incremental
```

**If aicx is not installed**, skip this step and note the gap.
Memory is valuable but not blocking.

### Step 2: Read Recent Context

Check what memory was extracted:

```bash
aicx refs -H 168 -p <project_name>
```

Read the most recent 1-2 context files to understand:
- What was the last task worked on?
- Are there open TODOs or decisions pending?
- What signals were extracted (look for `[signals]` blocks)?

### Step 3: Open Eyes (loctree MCP)

Map the current state of the codebase. Run in this order:

1. **`repo-view(project)`** — health, hubs, languages, LOC, dead exports, cycles
2. **`focus(directory)`** — for the target module(s) relevant to the task (1-3 dirs)
3. **`follow(scope)`** — if repo-view flagged signals (dead, cycles, twins, hotspots)

This gives the agent structural awareness: what files matter, what depends
on what, where the risk is.

### Step 4: Produce Situational Report (Required)

After steps 1-3, produce this situational report to stdout:

```
## Session Init: <project>

### Memory (ai-contexters)
- Last activity: <date from refs>
- Open signals: <TODOs, decisions from context files>
- Sessions extracted: <count>

### Structure (loctree)
- Files: <N>, LOC: <N>, Languages: <list>
- Health: <cycles, dead exports, twins>
- Top hubs: <top 3 files by importers>
- Target scope: <focused dirs>

### Ready
Agent has memory and eyes. Proceeding with task.
```

## For Subagent Prompts

When delegating to subagents via vetcoders-spawn, include this preamble:

```
## Context Bootstrap

Use loctree MCP tools as the primary exploration layer:
- repo-view(project) first for overview
- slice(file) before modifying any file
- find(name) before creating new symbols
- impact(file) before deleting

Historical context from previous sessions is available at:
~/.ai-contexters/<project_name>/

List available context files with:
aicx refs -p <project_name> -H 168
```

## Quick Reference

| Step | Tool | What It Gives |
|------|------|--------------|
| Memory | `aicx all -p X --incremental` | Past decisions, TODOs, signals |
| Refs | `aicx refs -H 168 -p X` | Paths to stored context chunks |
| Eyes | `repo-view(project)` | Current structure + health |
| Focus | `focus(directory)` | Module-level detail |
| Signals | `follow(scope)` | Dead code, cycles, twins |

## Fallback

If **aicx** unavailable: skip memory steps, proceed with loctree only.
If **loctree MCP** unavailable: fall back to `loct --for-ai` CLI, then `rg --files`.
If **both** unavailable: read CLAUDE.md + README.md + recent git log. Announce gaps.

## Anti-Patterns

- Starting implementation without running init (blind coding)
- Running loctree but skipping aicx (no memory of past work)
- Reading every context file (context bloat) — read only the 1-2 most recent
- Skipping repo-view and jumping to grep (no structural map)

---

*Created by M&K (c)2026 VetCoders*
