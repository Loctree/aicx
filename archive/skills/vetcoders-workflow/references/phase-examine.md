# Phase 1: EXAMINE — Deep Loctree Examination

## Purpose

Build complete structural understanding before any code change.
Output feeds directly into Research questions and Implementation plans.

## Examination Workflow

### Step 1: Global Map

Run `repo-view(project)` to capture:
- Total files and LOC
- Language breakdown
- Health indicators (dead exports, cycles, twins, hotspots)
- Top hub files (highest importer count)

Record health signals — these become `follow()` targets later.

### Step 2: Scope Constraint

For each target module (max 3 directories):
- Run `focus(directory)` to see internal structure
- Note: file count, LOC per file, internal edges, external dependencies
- Identify which files are entry points vs internal helpers

Decision heuristic:
- If module has <10 files → examine all
- If module has 10-30 files → focus on hubs + files matching task keywords
- If module has >30 files → focus on hubs only, expand as needed

### Step 3: File-Level Context

For every file likely to be modified:
- Run `slice(file, consumers=true)` to get:
  - Direct dependencies (what this file imports)
  - Direct consumers (what imports this file)
  - Transitive risk assessment
- Record dependencies and consumers in CONTEXT.md

### Step 4: Risk Assessment

For high-hub files (>5 consumers) and deletion candidates:
- Run `impact(file)` to get full blast radius
- Direct consumers: files that will break immediately
- Transitive consumers: files affected through dependency chain
- If impact is high → plan isolation strategy (adapter, new variant, scoped module)

### Step 5: Symbol Verification

Before proposing any new types, functions, or constants:
- Run `find(name)` with regex support: `find("NewType|new_function")`
- Check for existing patterns to reuse
- Check for naming conflicts

### Step 6: Signal Pursuit

If repo-view flagged issues (dead exports, cycles, twins, hotspots):
- Run `follow(scope)` for relevant scopes
- Dead exports → candidates for cleanup during refactor
- Cycles → dependency ordering constraints
- Twins → potential deduplication opportunities
- Hotspots → files that change frequently (higher test coverage needed)

## CONTEXT.md Output Format

```markdown
# Examination: <slug>
Date: <YYYY-MM-DD>
Pipeline: .ai-agents/pipeline/<slug>/

## Repo Health
- Files: N | LOC: N | Languages: Rust, Swift, ...
- Dead exports: N flagged
- Cycles: N detected
- Health score: good/warning/critical

## Task Understanding
- User request: <original request>
- Interpreted scope: <what needs to change>

## Target Modules
### <module-1>
- Path: <dir>
- Files: N | LOC: N
- Entry points: <files>
- External consumers: <count>

## Critical Files (slice results)
| File | LOC | Dependencies | Consumers | Risk |
|------|-----|-------------|-----------|------|
| path/to/file.rs | 450 | 3 | 12 | HIGH |

## Existing Symbols
- `TypeA` — defined in path/file.rs:42 (14 consumers)
- `fn helper_b` — defined in path/other.rs:100 (used by 3 files)

## Risk Map
| File | Blast Radius | Mitigation |
|------|-------------|------------|
| contracts.rs | 24 transitive | Additive changes only |

## Open Questions (for Research phase)
1. <question about unknown API/pattern>
2. <question about best approach>

## Phase Decision
- [ ] Research needed — unknown: <what>
- [ ] Skip to Implement — well-understood domain
```

## Common Examination Patterns

### New Feature
Focus on: where feature integrates, existing patterns to follow, hub files affected.
Key tools: `repo-view` → `focus` (target module) → `slice` (integration points) → `find` (similar features).

### Bug Fix
Focus on: reproduction path, affected code path, test coverage.
Key tools: `slice` (buggy file) → `impact` (fix scope) → `find` (related symbols).

### Refactor
Focus on: full blast radius, dependency ordering, migration path.
Key tools: `repo-view` → `focus` → `impact` (every file to touch) → `follow(cycles)`.

### Integration
Focus on: boundary files, adapter patterns, configuration.
Key tools: `repo-view` → `focus` (boundary module) → `slice` (adapter files) → `find` (existing adapters).

## Fallback (loctree unavailable)

If loctree MCP is not available:
1. `rg --files | head -50` for file overview
2. `rg -l "pattern"` for consumer discovery
3. `rg "use |mod |pub " file.rs` for dependency tracing
4. Manual impact assessment via grep

Report: "Loctree unavailable — using grep fallback. Structural coverage: reduced."
