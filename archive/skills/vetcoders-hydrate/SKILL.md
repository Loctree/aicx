---
name: vetcoders-hydrate
version: 1.0.0
description: >
  Packaging and go-to-market hydration skill. Takes DoU audit findings and
  executes the non-code work that bridges the gap between "it works" and
  "someone can buy this." Generates marketplace listings, SEO fixes,
  distribution artifacts, onboarding flows, and landing page content.
  Trigger phrases: "hydrate", "package for market", "prepare for launch",
  "przygotuj do launchu", "fix the packaging gap", "marketplace listing",
  "nawodnij", "make it shippable", "go-to-market", "distribution",
  "SEO fix", "landing page", "onboarding", "completion sprint".
---

# vetcoders-hydrate — The Antidote to Always-in-Production

> "The code is dry — structurally complete but missing the fluid
> that makes it flow to users. Hydration means: make the path
> from stranger to user frictionless."

The Hydrate skill is the packaging agent that DoU called for.
It treats "create a DMG installer" and "write SEO-friendly copy"
as first-class engineering tasks, not afterthoughts.

## When To Use

- After a `vetcoders-dou` audit identifies packaging gaps
- Before any marketplace submission or public launch
- When the team says "it works, now make it findable/installable/buyable"
- Periodic hydration sprint (recommended: pair with DoU every 2 weeks)
- When Plague Score > 40

## Hydration Domains

### Domain 1: Repository Hydration

Fix repo governance gaps identified by DoU:

**File Generation:**
```
For each missing file, generate contextually appropriate content:

LICENSE:
- Detect project intent (commercial vs open-source vs dual)
- Generate appropriate license (MIT, Apache-2.0, or proprietary)

CONTRIBUTING.md:
- Extract from existing README if present
- Include: setup, coding standards, PR process, code of conduct link
- Match repo language and toolchain

CHANGELOG.md:
- Parse git log for unreleased changes
- Format as Keep a Changelog standard
- Include version headers matching published versions

SECURITY.md:
- Standard responsible disclosure template
- Contact method (GitHub Security Advisories preferred)

CI Workflows:
- Detect language → generate appropriate CI
- Rust: cargo check, clippy, test, fmt
- Node: lint, test, build
- Python: ruff, pytest
- Always include: dependency audit, license check
```

**Version Synchronization:**
```bash
# Find all version references and check consistency
grep -rn "version" Cargo.toml package.json pyproject.toml
# Compare with published versions
cargo search <crate-name> 2>/dev/null | head -1
npm view <package-name> version 2>/dev/null
# Compare with website badges/references
# Flag any mismatch as P1
```

### Domain 2: Distribution Hydration

Make the product installable without a dev toolchain:

**CLI Tools (Rust):**
```
- [ ] cargo install <name> works
- [ ] GitHub Releases with prebuilt binaries (linux-x86_64, macos-arm64, macos-x86_64)
- [ ] Install script: curl -sSfL <url> | sh
- [ ] Homebrew formula (tap or core)
- [ ] Shell completions generated and included

Generate GitHub Actions release workflow:
- Cross-compile for targets
- Create GitHub Release with assets
- Update Homebrew formula automatically
```

**Desktop Apps (macOS):**
```
- [ ] .app bundle with proper Info.plist
- [ ] DMG with background image and Applications symlink
- [ ] Code signing with Developer ID
- [ ] Notarization via notarytool
- [ ] Homebrew cask formula
- [ ] Sparkle or equivalent for auto-updates

Template: create-dmg with:
  --volname "<AppName>"
  --background "dmg-background.png"
  --window-size 600 400
  --icon-size 100
  --app-drop-link 400 200
```

**Web Apps:**
```
- [ ] Dockerfile for containerized deployment
- [ ] docker-compose.yml for local preview
- [ ] Environment variable documentation (.env.example)
- [ ] Health check endpoint (/health or /api/health)
- [ ] Graceful shutdown handling
```

### Domain 3: Discoverability Hydration

Fix SEO and web presence gaps:

**SSR/Pre-rendering for SPA sites:**
```
Problem: JS-rendered sites are invisible to crawlers.
Solutions (in order of preference):
1. Static pre-rendering at build time (best for landing pages)
2. SSR with hydration (for dynamic content)
3. Hybrid: static landing + SPA for app
4. Minimum: <noscript> fallback with key content

For Leptos (WASM) sites:
- Enable SSR mode or generate static HTML
- Pre-render critical routes at build time
- Ensure <title>, <meta>, <h1> exist in initial HTML
```

**Meta Tags Generator:**
```html
<!-- Generate for each public page -->
<title>{Product} — {Tagline} | {Company}</title>
<meta name="description" content="{Value prop in 155 chars}">
<meta name="keywords" content="{5-8 relevant keywords}">

<!-- Open Graph -->
<meta property="og:title" content="{Title}">
<meta property="og:description" content="{Description}">
<meta property="og:image" content="{Social preview image URL}">
<meta property="og:type" content="website">

<!-- Twitter Card -->
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="{Title}">
<meta name="twitter:description" content="{Description}">

<!-- Security Headers (add to server config) -->
Strict-Transport-Security: max-age=63072000; includeSubDomains
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
Content-Security-Policy: default-src 'self'
```

**robots.txt + sitemap.xml:**
```
Generate from actual URL structure.
Ensure no duplicate content across domains.
Submit to Google Search Console (manual step — flag for user).
```

### Domain 4: Commercial Surface Hydration

Build the stranger-to-customer path:

**Landing Page Content Generation:**
```
Structure:
1. Hero: {Tagline} + {1-sentence value prop} + {Primary CTA}
2. Problem: {Pain point in user's words}
3. Solution: {How product solves it, 3 bullets max}
4. Social proof: {Stats, testimonials, case studies}
5. How it works: {3-step visual flow}
6. Pricing: {Clear tiers or "contact us"}
7. CTA repeat: {Same primary CTA}

Generate as:
- Markdown (for static site generators)
- HTML (for direct use)
- Copy document (for designer handoff)
```

**Marketplace Listing Generator:**

For Claude Code Skills Marketplace:
```markdown
# {Skill Name}

{One-line description}

## What it does
{2-3 sentences explaining the value}

## When to use
{Bullet list of trigger scenarios}

## How it works
{Brief technical explanation}

## Requirements
- {Required tools/dependencies}
- {Optional enhancements}

## Part of
{Suite name} — {suite description}
```

For crates.io / npm / PyPI:
```
- description: {<250 chars, keyword-rich}
- keywords: {5 relevant terms}
- categories: {matching registry categories}
- homepage: {landing page URL}
- repository: {GitHub URL}
- documentation: {docs URL}
- readme: {path to README}
```

### Domain 5: Onboarding Hydration

Create the "first 5 minutes" experience:

```
For CLI tools:
1. Install command (one line, copy-pasteable)
2. First command to run (shows immediate value)
3. "What just happened" explanation
4. Next steps (2-3 progressive commands)
5. Where to get help

For web apps:
1. Signup flow (< 3 fields)
2. Onboarding wizard (< 5 steps)
3. Sample data or demo mode
4. Quick win within 60 seconds
5. Documentation link

For skills/plugins:
1. Install command
2. Trigger phrase to test
3. Expected output
4. Customization options
```

## Hydration Sprint Protocol

When running a hydration sprint:

### 1. Ingest DoU Report
Read the DoU report. Extract all P0 and P1 findings.
Sort by impact (commercial surface > discoverability > repo governance).

### 2. Triage into Domains
Map each finding to a hydration domain (1-5).
Some findings map to multiple domains — list all.

### 3. Generate Artifacts
For each finding, generate the appropriate artifact:
- Missing files → create them
- Missing meta tags → generate HTML
- Missing install path → create CI workflow
- Missing landing content → write copy
- Missing marketplace listing → generate listing

### 4. Verify via DoU
After hydration, re-run `vetcoders-dou` on affected areas.
Target: Plague Score reduction of at least 20 points.

### 5. Present to User
```
## Hydration Report: <project>

### Before (Plague Score: XX)
<Undone Matrix from DoU>

### Artifacts Generated
| Domain | Artifact | Status |
|--------|----------|--------|
| Repo | LICENSE, CONTRIBUTING.md | [DONE] Created |
| Distribution | release.yml workflow | [DONE] Created |
| SEO | Meta tags for landing | [DONE] Generated |
| Commercial | Marketplace listing | [DONE] Written |
| Onboarding | Quick start guide | [DONE] Written |

### After (Plague Score: XX)
<Updated Undone Matrix>

### Remaining Manual Steps
<things only a human can do: DNS, API keys, marketplace submit button>
```

## Integration with VetCoders Pipeline

```
Phase 1 — Build:     init → workflow → followup
                                         ↓
Phase 2 — Converge:                  marbles ↻ (loop until P0=P1=P2=0)
                                         ↓
Phase 3 — Ship:                      dou → hydrate
```

Hydration is the **terminal skill** — it produces artifacts that cross the boundary
from code to market. After hydration, re-run DoU to verify the gap closed.

## Subagent Delegation

For large hydration sprints, delegate domains to subagents:

```
Agent 1: Repo Hydration (LICENSE, CONTRIBUTING, CI, CHANGELOG)
Agent 2: Distribution Hydration (release workflows, installers)
Agent 3: Discoverability Hydration (SEO, meta tags, pre-rendering)
Agent 4: Commercial Hydration (landing copy, marketplace listings)
```

Use `vetcoders-spawn` spawn pattern. Each agent gets:
- DoU findings for their domain
- Template artifacts from this skill
- Living tree preamble (standard)

## Anti-Patterns

- Hydrating without a DoU audit first (fixing what you assume, not what's measured)
- Generating files without repo context (LICENSE type must match project intent)
- Writing marketing copy without understanding the product (run vetcoders-init first)
- Treating hydration as one-off (it's a recurring sprint, like refactoring)
- Hydrating everything at once (prioritize: P0 commercial gaps first)
- Forgetting to re-run DoU after hydration (verify the fix)

## The "Done Done" Definition

A project is hydrated — truly "Done Done" — when:

```
[DONE] A stranger can DISCOVER it (search engines, marketplace, word of mouth)
[DONE] A stranger can UNDERSTAND it (landing page, README, value prop clear in 30s)
[DONE] A stranger can INSTALL it (one command, no dev toolchain, < 5 minutes)
[DONE] A stranger can USE it (onboarding, quick win within 60 seconds)
[DONE] A stranger can PAY for it (pricing, signup, trial — if commercial)
[DONE] A stranger can CONTRIBUTE (CONTRIBUTING.md, issue templates, CI — if open source)
```

Until all six are true, the project is in the Always-in-Production state.
Hydration is the antidote.

---

*"Hydration means: consolidate, give each product a complete surface,*
*make the path from stranger to user frictionless."*

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
