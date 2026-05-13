---
name: vetcoders-dou
version: 1.0.0
description: >
  Definition of Undone audit skill. Runs a systematic gap analysis across the
  ENTIRE product surface — not just code. Crawls public URLs, audits repo
  governance, verifies install paths, checks SEO/discoverability, and measures
  the gap between internal capability and external visibility.
  Trigger phrases: "definition of undone", "dou audit", "co jest niedokończone",
  "what's undone", "product surface audit", "completion audit", "plague check",
  "hydration check", "are we shippable", "czy jesteśmy gotowi", "gap analysis",
  "co brakuje do launchu", "readiness audit", "packaging gap".
---

# vetcoders-dou — Definition of Undone Audit

> "Green gates are necessary, not sufficient. Runtime truth wins."
> "The engineering is done. The packaging is not."

The DoU skill answers the question no agent asks by default:
**"What remains incomplete across the entire product surface, and why?"**

This is the completion oracle described in the DoU manifesto — not a one-off report,
but a structured, repeatable audit that measures the gap between "it runs on my machine"
and "someone can buy this."

## When To Use

- Before any launch, marketplace submission, or PR announcement
- After major implementation cycles (post vetcoders-followup)
- When the team asks "are we ready?" or "co jeszcze brakuje?"
- Periodic health check (recommended: every 2 weeks)
- When the feeling of progress exceeds the reality of completion

## The Undone Matrix

Every project is scored on three axes:

| Axis | Question | Tools |
|------|----------|-------|
| **Repo Health** | Does the code work? | loctree, cargo/npm, CI |
| **Web Presence** | Can someone find and understand it? | WebFetch, brave-search, curl |
| **Commercial Readiness** | Can someone adopt or buy it? | Manual checklist + automated probes |

Scoring: [OK] Production-ready | [PARTIAL] Exists but incomplete | [MISSING] Absent

## Audit Sequence

### Phase 1: Repo Governance Audit

Check repository health beyond code quality:

```
Required files:
- [ ] LICENSE (not just exists — correct license for intended use)
- [ ] README.md (with install, usage, and contributing sections)
- [ ] CONTRIBUTING.md (or contributing section in README)
- [ ] CHANGELOG.md (or GitHub Releases with notes)
- [ ] CI configuration (.github/workflows/ or equivalent)
- [ ] Issue templates (.github/ISSUE_TEMPLATE/)
- [ ] Security policy (SECURITY.md)
```

**Automated checks:**
```bash
# File existence
for f in LICENSE README.md CONTRIBUTING.md CHANGELOG.md SECURITY.md; do
  [ -f "$ROOT/$f" ] && echo "[PASS] $f" || echo "[FAIL] $f MISSING"
done

# CI presence
[ -d "$ROOT/.github/workflows" ] && echo "[PASS] CI workflows" || echo "[FAIL] No CI"

# Issue templates
[ -d "$ROOT/.github/ISSUE_TEMPLATE" ] && echo "[PASS] Issue templates" || echo "[FAIL] No issue templates"
```

**Loctree structural check:**
```
repo-view(project) → extract:
- Dead exports count (should be 0 for release)
- Cycle count (should be 0 or documented)
- Health score
```

### Phase 2: Install Path Verification

The "can a stranger use this" test:

```
For CLI tools:
- [ ] Published to package registry (crates.io / npm / PyPI)
- [ ] Version badge matches actual published version
- [ ] Install command works: cargo install <name> / npm i -g <name> / pip install <name>
- [ ] Binary runs after install without dev toolchain
- [ ] --help produces useful output
- [ ] --version matches published version

For desktop apps:
- [ ] DMG/MSI/AppImage available
- [ ] Homebrew formula or equivalent
- [ ] Code signing / notarization (macOS)
- [ ] No dev toolchain required to install

For web apps:
- [ ] URL is accessible
- [ ] Loads in <3 seconds
- [ ] Works without JavaScript (or graceful fallback message)
- [ ] Mobile responsive
```

### Phase 3: Discoverability Audit

The "can Google find us" test:

**URL Crawl (for each public URL):**
```
1. WebFetch(url, "Extract: page title, meta description, h1, main content summary,
   any CTA buttons, pricing info. Report if page appears empty or JS-only.")

2. Check for SSR/static content:
   - curl -s <url> | grep -c '<h1\|<p\|<main'
   - If count < 3 → page is likely JS-rendered and invisible to crawlers

3. Security headers:
   - curl -sI <url> | grep -i 'strict-transport\|x-frame\|content-security\|x-content-type'

4. Open Graph / social sharing:
   - curl -s <url> | grep -i 'og:title\|og:description\|og:image\|twitter:card'
```

**SEO basics:**
```
- [ ] Title tag present and descriptive (not "React App")
- [ ] Meta description present
- [ ] H1 exists and matches page purpose
- [ ] Content visible without JavaScript
- [ ] robots.txt allows indexing
- [ ] sitemap.xml exists
- [ ] No duplicate content across domains (e.g., loct.io vs loctree.io)
```

**Search presence:**
```
brave-search("<product name>") → check if product appears in top 20
brave-search("<product name> <category>") → check category ranking
brave-search("site:<domain>") → check indexed page count
```

### Phase 4: Commercial Surface Audit

The "stranger to customer" path test:

```
Discovery → Landing → Understanding → Trial → Adoption → Payment

For each stage, verify:
- [ ] Discovery: Can be found via search (Phase 3)
- [ ] Landing: Page loads, content visible, professional appearance
- [ ] Understanding: Features, use cases, and value prop are clear within 30s
- [ ] Trial: Demo, free tier, or try-before-buy available
- [ ] Adoption: Install/signup path works end-to-end
- [ ] Payment: Pricing page exists (if commercial)

Missing stages = holes in the funnel. Report each gap.
```

### Phase 5: Marketplace Readiness (if targeting Claude/AI ecosystem)

```
Claude Code Skills Marketplace:
- [ ] SKILL.md with proper frontmatter (name, version, description)
- [ ] Description contains trigger phrases (EN + PL if applicable)
- [ ] References directory with supporting docs
- [ ] No hardcoded paths (uses $ROOT or relative paths)
- [ ] Works without optional dependencies (graceful fallback)
- [ ] Tested on clean install (no assumed global state)

GitHub Marketplace / crates.io / npm:
- [ ] Package metadata complete (description, keywords, homepage, repository)
- [ ] Categories/tags set correctly
- [ ] Screenshots or demo GIF in README
- [ ] License compatible with marketplace requirements
```

## Output Format

### DoU Report (Mandatory Structure)

```markdown
# Definition of Undone: <project/ecosystem>
Date: <YYYY-MM-DD>
Auditor: <agent identifier>

## Executive Summary
<2-3 sentences: what's the gap between code and market?>

## Undone Matrix
| Project | Repo | Web | Commercial | Critical Gap |
|---------|------|-----|------------|-------------|
| <name>  | [OK]/[PARTIAL]/[MISSING] | [OK]/[PARTIAL]/[MISSING] | [OK]/[PARTIAL]/[MISSING] | <one line> |

## Findings by Severity

### P0 — Ship Blockers
<issues that prevent any stranger from discovering/using the product>

### P1 — Credibility Gaps
<issues that make the product look unfinished or untrustworthy>

### P2 — Polish Items
<nice-to-have improvements for professional appearance>

## The Funnel Test
Discovery → Landing → Understanding → Trial → Adoption → Payment
<for each product, mark where the funnel breaks>

## Hydration Priorities
<ordered list of what to fix first, with estimated effort>

## Plague Score
<0-100: how affected is this project by the Always-in-Production Plague?>
0 = fully shipped and discoverable
100 = technically brilliant, commercially invisible
```

## Integration with VetCoders Pipeline

```
Phase 1 — Build:     init → workflow → followup
                                         ↓
Phase 2 — Converge:                  marbles ↻ (loop until P0=P1=P2=0)
                                         ↓
Phase 3 — Ship:                      dou → hydrate
```

DoU findings feed directly into `vetcoders-hydrate` as prioritized tasks.

## Anti-Patterns

- Running DoU only on code (it's a PRODUCT surface audit, not a code audit)
- Treating [OK] repo health as proof of readiness (the whole point of DoU)
- Auditing without actually crawling URLs (trust no assumption)
- Skipping the install path test ("it works on my machine" is the plague)
- Reporting without severity ranking (everything feels equally important)
- Not re-running after hydration (verify the fix)

## The Plague Diagnostic

If you observe this pattern, the project has the plague:
1. Tests pass [PASS]
2. Architecture is sound [PASS]
3. README exists [PASS]
4. Nobody can find it from Google [FAIL]
5. Nobody can install it without a toolchain [FAIL]
6. Nobody can pay for it [FAIL]

Items 4-6 are the Definition of Undone.

---

*"The antidote is not more tools. It is not another framework.*
*It is a decision: choose what ships, and finish it. All of it. Not just the code."*

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
