---
name: vetcoders-implement
version: 1.0.0
description: >
  Native Claude subagent delegation skill using Claude's built-in Task tool.
  Safe, sandboxed alternative to vetcoders-spawn (osascript Terminal method).
  Delegates implementation, research, and audit tracks to Claude subagents
  that run within the same session — no Terminal windows, no --dangerously flags,
  no osascript. Less robust but universally safe.
  Trigger phrases: "implement with agents", "delegate to subagents", "zaimplementuj",
  "run agents", "parallel tasks", "delegate safely", "native agents",
  "Task tool agents", "implement plan", "uruchom agentów", "subagenty natywne",
  "bezpieczne agenty", "implement without spawn", "no osascript".
---

# vetcoders-implement — Native Claude Subagent Delegation

> Safe parallel delegation using Claude's built-in Task tool.
> The civilized sibling of `vetcoders-spawn`.

## When To Use

Use this skill when you want parallel agent delegation **without** Terminal spawn:

- Working in Cowork mode, Claude.ai, or any environment without Terminal.app
- When `--dangerously-skip-permissions` is not acceptable
- When you want sandboxed execution with built-in safety
- When the team prefers visibility in the conversation over Terminal windows
- When osascript is unavailable (Linux, remote, CI)

**If you need** full Terminal isolation, persistent processes, or Codex agents →
use `vetcoders-spawn` instead.

## Comparison: implement vs spawn

| Capability | vetcoders-implement | vetcoders-spawn |
|-----------|-------------------|----------------|
| **Execution** | Claude Task tool (in-process) | osascript Terminal (out-of-process) |
| **Safety** | Sandboxed, no dangerous flags | Requires `--dangerously-*` flags |
| **Agents** | Claude subagents only | Claude + Codex + any CLI agent |
| **Parallelism** | Multiple Task calls in one message | Multiple Terminal windows |
| **Persistence** | Within conversation context | Independent processes |
| **Visibility** | Results returned to conversation | Must read report files |
| **Robustness** | Limited by context window | Full agent session per task |
| **Environment** | Inherits parent env | Clean Terminal env |
| **Best for** | Research, audit, small impl | Large impl, multi-agent fleets |

## Standard Workflow

### 1. Prepare Pipeline Structure

Same convention as `vetcoders-spawn`:

```bash
ROOT="$(git rev-parse --show-toplevel)"
SLUG="$(date +%Y%m%d)_<task-name>"

mkdir -p "$ROOT/.ai-agents/pipeline/$SLUG/plans"
mkdir -p "$ROOT/.ai-agents/pipeline/$SLUG/reports"
```

### 2. Write Plans

Write one plan per subagent to `.ai-agents/pipeline/<slug>/plans/`.
Use the same plan template as `vetcoders-spawn`:

```markdown
# Task: <short title>

Goal:
- <1-3 bullets>

Scope:
- In scope: <files/areas>
- Out of scope: <explicit>

Constraints:
- No --no-verify
- Follow repo conventions

Acceptance:
- [ ] <objective outcome>
- [ ] <objective outcome>
- [x] refinement

Test gate:
- <command(s)>

Context:
- <from CONTEXT.md + RESEARCH.md>

Living tree note:
- You work on a living tree with Vibecrafting methodology, so concurrent changes are expected.
- Adapt proactively and continue, but this is never permission to skip quality, security, or test gates.
- Run required checks. If something is blocked, report the exact blocker and run the closest safe equivalent.
```

### 3. Spawn via Task Tool

**The key difference**: instead of osascript, use Claude's native `Task` tool.

#### Single agent (sequential)

```
Task(
  subagent_type: "general-purpose",
  description: "<3-5 word summary>",
  prompt: "<full plan content from plan file>"
)
```

#### Multiple agents (parallel)

Launch multiple Task calls in a **single message** — Claude executes them concurrently:

```
# In one response, call multiple Task tools:

Task(subagent_type: "general-purpose", description: "Runtime audit agent",
     prompt: "<plan 1 content>")

Task(subagent_type: "general-purpose", description: "Settings polish agent",
     prompt: "<plan 2 content>")

Task(subagent_type: "general-purpose", description: "UI consistency agent",
     prompt: "<plan 3 content>")
```

All three run in parallel. Results return when each completes.

#### For code-heavy tasks (Bash agent)

```
Task(
  subagent_type: "Bash",
  description: "Run quality gates",
  prompt: "cd $ROOT && cargo clippy -- -D warnings && cargo test 2>&1"
)
```

#### For research tasks (Explore agent)

```
Task(
  subagent_type: "Explore",
  description: "Find auth patterns",
  prompt: "Search the codebase at $ROOT for authentication patterns.
           Find all files handling login, session, token management.
           Report: file paths, line counts, key functions."
)
```

### 4. Collect Results

Task tool returns results directly to the conversation.
**Also** write results to report files for pipeline continuity:

```
Write report to: .ai-agents/pipeline/<slug>/reports/<N>_<task>_claude-task.md
```

Format:

```markdown
# Report: <task title>
Agent: Claude Task (native)
Date: <YYYY-MM-DD>
Duration: <from Task output>

## Findings
<agent output, structured>

## Gate Results
<pass/fail per gate>

## Files Changed
<list with +/- line counts>
```

### 5. Synthesize

After all agents return, the primary agent (you):

1. Read all reports
2. Cross-reference findings
3. Run quality gates on current tree
4. Produce consolidated verdict

## Subagent Type Selection Guide

| Task Type | Subagent Type | Why |
|-----------|--------------|-----|
| Implementation (code changes) | `general-purpose` | Full tool access for read/write/edit |
| Quick search / symbol lookup | `Explore` | Fast, focused, read-only |
| Run tests / linters / builds | `Bash` | Direct command execution |
| Architecture planning | `Plan` | Read-only, design focused |
| Research (web + docs) | `general-purpose` | Needs WebSearch, WebFetch |

## Prompt Engineering for Task Agents

### Mandatory Preamble (include in every prompt)

```
## Context
Project: $ROOT
Pipeline: .ai-agents/pipeline/<slug>/

## Structural Intelligence (loctree MCP)
Use loctree MCP tools as your primary exploration layer:
- repo-view(project) first for codebase overview
- slice(file) before modifying any file
- find(name) before creating new symbols
- impact(file) before deleting
Never edit code without mapping it first.

## Living Tree
You work on a living tree with Vibecrafting methodology, so concurrent changes are expected.
Adapt proactively and continue, but this is never permission to skip quality, security, or test gates.
Run required checks. If something is blocked, report the exact blocker and run the closest safe equivalent.
```

### Context Injection Pattern

For agents that need pipeline context:

```
## From Examination (CONTEXT.md):
<paste relevant sections>

## From Research (RESEARCH.md):
<paste relevant sections>

## From Previous Reports:
<paste key findings from prior loop iterations>
```

### Keep Prompts Focused

Task agents have **limited context windows** compared to Terminal-spawned agents.
Rules:

- Max 1 CONTEXT.md section per agent (not the whole file)
- Max 3-5 files in scope per agent
- Specific acceptance criteria (not "make it better")
- One clear deliverable per agent

## Loop Pattern (for iterative polish)

The pattern from your CodeScribe flow, natively:

```
Loop N:
  1. Read known findings (from previous reports or DoU)
  2. Task(general-purpose): "Fix findings A, B, C in <scope>"
  3. Task(Bash): "Run quality gates: <commands>"
  4. Task(Explore): "Verify fixes didn't break consumers of <files>"
  5. Write consolidated report
  6. If P0/P1 remain → Loop N+1
  7. If only P2 → present verdict, ask user
```

Implemented as parallel Tasks where possible:

```
# Step 2+3+4 can run in parallel:
Task("Fix agent runtime findings", prompt=<fix plan>)
Task("Fix settings findings", prompt=<fix plan>)
Task("Run gates", prompt="cd $ROOT && cargo clippy -- -D warnings")
```

## Limitations (vs vetcoders-spawn)

Be honest about what you lose:

1. **No Codex agents** — Task tool only spawns Claude subagents
2. **No persistent sessions** — agents live within conversation, not as independent processes
3. **Context ceiling** — each agent shares the parent's context budget
4. **No env isolation** — agents inherit parent's environment (including `CLAUDECODE`)
5. **No real parallelism** — concurrent Tasks, but single-threaded execution per agent
6. **No stdin piping** — can't pipe plan files directly, must include in prompt

**Mitigation**: For tasks exceeding these limits, escalate to `vetcoders-spawn`.

## When to Escalate to vetcoders-spawn

Switch to `vetcoders-spawn` when:

- Task requires >5000 tokens of output per agent
- Need Codex agents (better for pure implementation)
- Need true process isolation (env vars, PATH, toolchain)
- Running >3 agents in parallel (context contention)
- Agent needs to install dependencies or modify system state
- Task will take >10 minutes of agent time

## Integration with VetCoders Pipeline

```
vetcoders-init → vetcoders-workflow → vetcoders-implement (or spawn) → vetcoders-followup
                                              ↑                               ↓
                                              └─── loop if findings ──────────┘
                                                                              ↓
                                                                        vetcoders-dou
                                                                              ↓
                                                                      vetcoders-hydrate
```

`vetcoders-implement` and `vetcoders-spawn` are **interchangeable** in the pipeline.
The pipeline doesn't care HOW agents were spawned — only that plans exist in
`/plans/` and reports exist in `/reports/`.

## Output Convention

Same as `vetcoders-spawn` — full compatibility:

- Plans: `.ai-agents/pipeline/<slug>/plans/<N>_<task>_claude-task.md`
- Reports: `.ai-agents/pipeline/<slug>/reports/<N>_<task>_claude-task.md`
- Agent suffix: `_claude-task` (vs `_codex` or `_claude` for spawn)

## Anti-Patterns

- Using implement for tasks that clearly need spawn (large refactors, multi-repo)
- Sending entire CONTEXT.md as prompt (context bloat — send relevant sections only)
- Not writing reports to files (breaks pipeline continuity)
- Running >5 parallel Tasks (diminishing returns, context contention)
- Skipping loctree preamble in prompts (proven 37% quality drop)
- Using Bash agent for implementation (use general-purpose — Bash can't edit files)

## Safety Advantages

Why this exists alongside `vetcoders-spawn`:

1. **No `--dangerously-skip-permissions`** — all actions go through standard permission model
2. **No `--dangerously-bypass-approvals-and-sandbox`** — sandboxed by default
3. **No `unset CLAUDECODE` hacks** — Task tool handles nesting natively
4. **No osascript** — works on any OS, any environment
5. **Auditable** — all agent actions visible in conversation history
6. **Interruptible** — user can stop any agent mid-execution

> *"The same smart, same capable — just safer."*

---

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
