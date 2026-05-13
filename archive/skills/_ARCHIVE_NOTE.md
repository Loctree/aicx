---
archive: vetcoders-skill-suite-feb-2026
era: pre-rebrand
captured_at: 2026-05-08
captured_by: claude (vc-ownership /loop session)
authority: RepoVerified
---

# VetCoders skill suite — pre-rebrand archive (Feb–Mar 2026)

This directory captures the **original `vetcoders-*` skill suite** as it
existed in `aicx` repo before the framework rename to `vibecrafted` and
the skill prefix shift `vetcoders-*` → `vc-*`. Folder mtimes range
2026-02-27 → 2026-03-08.

It was discovered 2026-05-08 sitting untracked under `aicx/skills/`,
gitignored away by `.gitignore`'s `/skills` rule. The operator briefly
removed the rule to surface it for archeology, then asked it be
preserved here so the lineage stops being a silent inode.

## Why archive (not delete)

The skill suite is **doctrine-grade history**. Today's `vc-*` skills
inherit names, mental models, and trigger phrases from these earlier
forms. Reading them side-by-side shows precisely how the framework
sharpened:

- `vetcoders-init` → `vc-init` — neutral "give context" → post-vibe-coding triage doctrine
- `vetcoders-spawn` → `vc-agents` — osascript Terminal hack → proper agent fleet abstraction
- `vetcoders-subagents` → `vc-delegate` — how-to script → decision doctrine
- `vetcoders-ship` → `vc-release` — pipeline orchestrator → publish-readiness gate (security/DNS/SEO/verification)
- `vetcoders-marbles` → `vc-marbles` — "noise scheduler" metaphor → "truth-convergence executor" operational
- `vetcoders-implement` → `vc-implement` — delegation tool → end-to-end delivery owner

Plus 9 brand-new skills appeared between Mar and May 2026:
`vc-ownership`, `vc-partner`, `vc-research`, `vc-review`, `vc-scaffold`,
`vc-prune`, `vc-intents`, `vc-polarize` (added 2026-05-08, day of this
archive), and the `vc-justdo` legacy alias.

5 utility skills (`ai-contexters`, `bravesearch`, `loctree`, `pdf`,
`docs`) were lifted out of the bundle during the rebrand and live now
either as standalone user-skills (`~/.claude/skills/<name>/`) or were
absorbed into the MCP layer (brave-search MCP, loctree-mcp).

## Provenance

Source: `aicx` repository, branch `feat/aicx-extract-improvements`,
present in working tree as of 2026-05-08 ~21:35 local time. Folder
contained 31 files across 16 top-level directories plus
`vetcoders-suite-showcase.html` (46 KB rebrand-era marketing landing)
and `README.md` (6.8 KB suite description).

The current canonical skill source-of-truth lives at
`vc-runtime/vibecrafted/skills/` with the `vc-*` naming. This archive
is **not** the install path — operators install skills via vibecrafted
(`vc-runtime/vibecrafted/skills/`) and Claude Code surface
(`~/.claude/skills/`).

## Authority

`RepoVerified` — every file mtimes / structure was preserved verbatim
during `mv skills archive/skills`. No content edits.

## Redaction note (2026-05-08)

`bravesearch/brave_search.py` historically hardcoded a real Brave Search
API key (line 17, 32-char token). Two-step cleanup:

1. **Initial redact** of the constant to env-var lookup — caught the
   leak before any `git push` could publish it.
2. **Drop the file entirely.** A `.py` script is implementation detail,
   not doctrine. The doctrine value of the bravesearch skill lives in
   the sibling `SKILL.md` (description, trigger phrases, scope). The
   script was a how-to artifact that does not need to be archived. The
   alternative — keeping it with `archive/` excluded from semgrep —
   would be a silencer (per `vc-prune` skill: rip every silencer). So
   the file is dropped, semgrep gates remain active across the whole
   repo, and live security policy stays uniform.
3. **Rotated upstream** by the operator at the Brave Search dashboard.
   The leaked token is no longer accepted.

The pre-redaction commit (`7252629`) was unpushed when the leak was
caught, so the leaked token never entered the published git history of
this repo. Conversation transcript that surfaced the value has been
flagged separately for any necessary downstream cleanup (aicx corpus,
dispatched-agent contexts, etc.).

Future rule: archive content must pass the same security gates as live
code. If an archive contender carries hardcoded secrets, the secret
must be removed (or the file dropped) before the archive lands —
silencers in `.semgrepignore` are not a substitute.

## Out of scope

- This archive is **frozen**. Do not edit `vetcoders-*` files here.
  Doctrine evolution happens in `vc-runtime/vibecrafted/skills/<vc-*>/`.
- This archive is **not** a fallback skill source. If a tool tries to
  load skills from here, that's a wiring bug — fix the wiring.

`𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍.` with AI Agents by VetCoders (c)2024-2026 LibraxisAI
