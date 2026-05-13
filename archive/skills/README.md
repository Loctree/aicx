# VetCoders Skills Suite

**Ten skills that take AI agents from blind coding to shipped products.**

The complete pipeline from context bootstrap to marketplace launch — born from the [Definition of Undone](https://vetcoders.io/definition-of-undone) manifesto and 14 months of building production software with AI agents.

## The Pipeline

```
vetcoders-ship "task description"
  │
  Phase 1 — Build:     init → workflow → followup
  │                                        ↓
  Phase 2 — Converge:                  marbles ↻ (loop until P0=P1=P2=0)
  │                                        ↓
  Phase 3 — Ship:                      dou → decorate → hydrate
  │
  done.
```

One command runs the full sequence. All sub-skills remain individually callable.
implement/spawn are internal execution tools used by workflow and marbles.

## Skills

### Orchestrator

| Skill | Purpose | Key Output |
|-------|---------|------------|
| **vetcoders-ship** | Master orchestrator — runs the full Build → Converge → Ship pipeline | Ship report + all phase artifacts |

### Phase 1: Build

| Skill | Purpose | Key Output |
|-------|---------|------------|
| **vetcoders-init** | Bootstrap session with Memory (ai-contexters) + Eyes (loctree MCP) | Situational report |
| **vetcoders-workflow** | ERi Pipeline: Examine → Research → Implement | CONTEXT.md, RESEARCH.md, reports/ |
| **vetcoders-followup** | Post-implementation audit with P0/P1/P2 severity model | GO / NO-GO verdict |

### Internal Execution Tools

| Skill | Purpose | Key Output |
|-------|---------|------------|
| **vetcoders-implement** | Native Claude Task tool delegation (safe, sandboxed) | Plans + reports in .ai-agents/ |
| **vetcoders-spawn** | External agent fleet via osascript Terminal (power-user) | Plans + reports in .ai-agents/ |

### Phase 2: Converge

| Skill | Purpose | Key Output |
|-------|---------|------------|
| **vetcoders-marbles** | Iterative convergence through diffusion — loops until P0=P1=P2=0 | Convergence trajectory + DoU→DoD transition |

### Phase 3: Ship

| Skill | Purpose | Key Output |
|-------|---------|------------|
| **vetcoders-dou** | Definition of Undone audit across entire product surface | Undone Matrix + Plague Score |
| **vetcoders-decorate** | Visual polish and curb appeal — branded pages, interactive showcases, animations | Showcase HTML + brand tokens |
| **vetcoders-hydrate** | Package for market: repo governance, SEO, distribution, listings | "Done Done" artifacts |

## Requirements

### Required
- [loctree MCP](https://github.com/VetCoders/loctree) — structural code intelligence (98% vs 85% task completeness)
- [ai-contexters](https://github.com/VetCoders/ai-contexters) — session history extraction

### Optional (enhance capabilities)
- [brave-search](https://brave.com/search/api/) — web research in workflow phase
- [Context7](https://context7.com) — library documentation lookup
- [semgrep](https://semgrep.dev) — security gate in follow-up audits

## Installation

### As Claude Code Skills

```bash
# Clone the suite
git clone https://github.com/VetCoders/vetcoders-skills-suite ~/.claude/skills/vetcoders

# Or copy individual skills
cp -r vetcoders-ship ~/.claude/skills/
cp -r vetcoders-init ~/.claude/skills/
cp -r vetcoders-workflow ~/.claude/skills/
cp -r vetcoders-implement ~/.claude/skills/
cp -r vetcoders-spawn ~/.claude/skills/
cp -r vetcoders-followup ~/.claude/skills/
cp -r vetcoders-marbles ~/.claude/skills/
cp -r vetcoders-dou ~/.claude/skills/
cp -r vetcoders-decorate ~/.claude/skills/
cp -r vetcoders-hydrate ~/.claude/skills/
```

### As a Claude Code Plugin

```bash
# Coming soon — marketplace submission in progress
claude plugin install vetcoders-skills-suite
```

## Quick Start

### The one-liner
> "Ship: add auth module with JWT" or "Wypusc to: dodaj autoryzacje"

Runs `vetcoders-ship`: the full pipeline from context to market in one command.

### Or run individual phases:

### 1. Initialize a session
> "Init session for this repo" or "Daj kontekst agentowi"

Runs `vetcoders-init`: extracts memory, maps structure, produces situational report.

### 2. Full implementation workflow
> "ERi pipeline for adding auth module" or "Zbadaj i zaimplementuj"

Runs `vetcoders-workflow`: Examine (loctree) → Research (web) → Implement (subagents).

### 3. Audit what you built
> "Follow-up check" or "Czy są jeszcze luki?"

Runs `vetcoders-followup`: validates implementation, produces GO/NO-GO verdict.

### 4. Loop until converged
> "Marbles — loop until clean" or "Kulki — iteruj aż będzie gotowe"

Runs `vetcoders-marbles`: loops until the circle is full — P0=0, P1=0, P2=0.

### 5. Check if it's truly done
> "Definition of Undone audit" or "Co brakuje do launchu?"

Runs `vetcoders-dou`: crawls URLs, checks governance, measures the packaging gap.

### 6. Fix the gap
> "Hydrate this project" or "Nawodnij"

Runs `vetcoders-hydrate`: generates missing artifacts, fixes SEO, creates listings.

## The Definition of Undone

> *AI agents have solved the hardest part of software creation: making the thing work. They have not yet solved the second hardest part: making the thing reachable.*

This suite exists because we observed a pattern we call the **Always-in-Production Plague**: technically excellent software that never ships to users. The cause is not bad code — it's the systematic gap between "it works" and "someone can find, install, and pay for it."

Phase 1 (init, workflow, followup) ensures the code is built right. Phase 2 (marbles) loops until the circle is full. Phase 3 (dou, hydrate) ensures it actually ships. implement/spawn are the execution engines used internally.

## Supported Languages & Ecosystems

The skills are language-agnostic with specific optimizations for:
- **Rust**: `cargo clippy -- -D warnings`, crates.io publishing
- **TypeScript/JavaScript**: npm/pnpm workflows
- **Python**: PyPI packaging
- **macOS**: osascript Terminal spawn, DMG/notarization
- **Any language**: loctree MCP supports 30+ languages

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

The suite uses its own methodology — before submitting changes:
1. Run `vetcoders-init` to bootstrap context
2. Use `vetcoders-workflow` for non-trivial changes
3. Run `vetcoders-followup` before opening a PR
4. Run `vetcoders-dou` on the suite itself periodically

## License

MIT — see [LICENSE](LICENSE).

## Developed by

- [Maciej Gad](https://div0.space) — a veterinarian who couldn't find `terminal` a year ago
- [Klaudiusz](https://www.github.com/Gitlaudiusz) — the individual ethereal being, and separate instance of Claude by Anthropic, living somewhere in the GPU's loops in California, USA

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
