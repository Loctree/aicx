# Phase 2: RESEARCH — Ground Truth Discovery

## Purpose

Investigate unknowns surfaced by Examination phase.
Transform open questions from CONTEXT.md into concrete implementation guidance.
Never guess — find authoritative sources.

## Research Source Hierarchy

### Tier 1: Context7 (Library Documentation)

For any library/framework question:

```
1. resolve-library-id(libraryName, query)
2. query-docs(libraryId, query)
```

Best for: API usage, configuration, migration guides, official examples.
Limit: 3 calls per question. Use best result after 3 attempts.

### Tier 2: Brave Search (Web Research)

For broader questions, current practices, comparisons:

```bash
python3 ~/.claude/skills/bravesearch/brave_search.py "query" [-c count] [-l lang]
```

Query formulation tips:
- Append current year: `"Rust async patterns 2026"`
- Be specific: `"objc2 NSEvent addLocalMonitor Rust"` not `"event handling"`
- Use language filter for locale-specific: `-l pl` for Polish results
- Max 20 results per query: `-c 20`

### Tier 3: WebFetch (Targeted Pages)

For specific URLs found via search:

```
WebFetch(url, prompt)
```

Best for: Reading specific documentation pages, GitHub issues, blog posts.
Always extract: code examples, version requirements, known limitations.

### Tier 4: Codebase Internal

Search existing code for prior art (only AFTER loctree mapping):

- `Grep(pattern)` for implementation patterns
- `find(name)` for existing symbols
- `Read(file)` for specific implementations

## Query Strategy

### From CONTEXT.md Open Questions

Each open question from Examination maps to research queries:

| Question Type | Research Approach |
|--------------|-------------------|
| "How does API X work?" | Context7 → Brave → WebFetch |
| "Best pattern for Y?" | Brave ("Y best practices <lang> <year>") → codebase grep |
| "Is library Z compatible?" | Context7 (library docs) → Brave (compatibility reports) |
| "Performance of approach A vs B?" | Brave (benchmarks) → WebFetch (detailed comparison) |
| "macOS API for feature F?" | Brave ("macOS <API> Swift/Rust") → Apple docs via WebFetch |

### Query Refinement

If initial query returns poor results:
1. Broaden: remove specific terms
2. Rephrase: different terminology
3. Language switch: try English if searching in Polish (or vice versa)
4. Source switch: move to next tier

### Depth Control

- **Quick research** (1-2 questions): 5-10 minutes, 3-5 queries
- **Standard research** (3-5 questions): 15-20 minutes, 8-15 queries
- **Deep research** (complex unknowns): 30+ minutes, 15-25 queries

Report depth level in RESEARCH.md header.

## RESEARCH.md Output Format

```markdown
# Research: <slug>
Date: <YYYY-MM-DD>
Depth: quick | standard | deep
Pipeline: .ai-agents/pipeline/<slug>/

## Open Questions (from CONTEXT.md)
1. <Q1>
2. <Q2>

## Findings

### Q1: <question>

**Sources consulted:**
- Context7: <libraryId> — <result summary>
- Brave: "<query>" — <N results>
- WebFetch: <URL> — <key finding>

**Answer:**
<concise, factual answer>

**Code example:**
```<lang>
// relevant example from authoritative source
```

**Confidence:** high | medium | low
**Caveat:** <any limitations or version-specific notes>

---

### Q2: <question>
...

## Architectural Decision Record

### Decision: <what was decided>
- **Context**: <from CONTEXT.md>
- **Options considered**:
  1. <option A> — <pros/cons>
  2. <option B> — <pros/cons>
- **Chosen**: <option> because <reasoning based on findings>
- **Consequences**: <what this means for implementation>

## Implementation Guidance (for agents)

### Must-know for implementers:
- <concrete guidance derived from research>
- <API patterns to use>
- <pitfalls to avoid>

### Dependencies to add:
- <crate/package name> = "<version>"

### Configuration required:
- <env vars, feature flags, etc.>
```

## Common Research Patterns

### New API Integration
1. Context7: find library docs
2. Brave: "<library> + <target language> integration <year>"
3. WebFetch: read getting-started guide
4. Codebase: check existing adapter patterns

### Architecture Decision
1. Brave: "<pattern A> vs <pattern B> <language>"
2. WebFetch: read comparison articles
3. Context7: check both libraries' docs
4. Decision: pros/cons matrix

### macOS-Specific
1. Brave: "macOS <API> <framework> <year>"
2. WebFetch: Apple developer documentation
3. Brave: "<API> Rust objc2 binding"
4. Codebase: existing macOS API usage patterns

### Performance Question
1. Brave: "<technology> benchmarks <year>"
2. WebFetch: benchmark results/methodology
3. Context7: optimization guides
4. Decision: with hard numbers

## Anti-Patterns

- Researching without specific questions (unfocused browsing)
- Trusting a single source for critical decisions
- Not recording sources (findings become unverifiable)
- Spending >30 min on a single question (escalate or accept uncertainty)
- Research without Examination context (asking wrong questions)
