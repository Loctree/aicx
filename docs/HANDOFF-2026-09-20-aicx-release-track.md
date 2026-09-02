# Handoff — AICX release track (from the loctree 0.14.5 session)

> Written 2026-09-20 by kimi (session `fb3982fe-20a6-40d2-8d22-732b7b4c5252`),
> operator of the loctree 0.14.5 release. This handoff carries the working
> method that shipped that release, applied to AICX. Read it fully BEFORE
> touching anything — §1 and §2 are the difference between the good release
> and the average one.
>
> Reference session evidence: Loctree/loctree PRs #87 (mega-integration,
> squash `8f5cedf`), #88 (release cut, `3678b99`), tag `v0.14.5`;
> Loctree/loctree-com PRs #21, #22; live acid test:
> `curl -fsSL https://loctree.com/install.sh | sh` → `loct 0.14.5+g3678b992`.

## 0. The five tasks (from the Founder, verbatim intent)

1. Implement `make npm-install` for aicx in this repo.
2. Do a comprehensive release — make the flow as pleasant and coherent as
   loctree's was this time.
3. Review open PRs, merge them into ONE integration branch — exactly like
   here: same care, same rules (resolve ALL review comments, line-level
   conflict merging, never trash anyone's work — Monika's PRs included).
4. Review unmerged worktrees from this host, pull them into the integration
   branch from (3), then open a PR from that branch to main.
5. Bring the release surface all the way to npm publish inclusive.

Execute in order 1 → 5. Each task below has acceptance criteria. Tasks 3–4
feed task 2; task 5 closes task 2.

## 1. Orientation gate (vc-init) — first 15 minutes, non-negotiable

- **Map**: run `loct` (scan), then `loct context` for
  `/Volumes/vc-workspace/Loctree/aicx` and READ IT TO THE END — entrypoints,
  blast radius, twins, dead surfaces, the real shape. The repo is indexed by
  loctree; use `slice` before edits, `impact` before deletes/refactors,
  `find --literal` / `occurrences` / `body` for literal truth.
- **Intent**: `aicx extract` the relevant sessions — why the code became
  this, what was already tried (task 1's target repo is AICX itself; use it).
- **Ground truth**: `git status`, main vs origin/main, open PRs
  (`gh pr list --repo Loctree/aicx`), worktrees (`git worktree list`),
  dirty files. The checkout currently sits on `feat/index-single-command`
  (PR #81). Root contains WIP junk (`fix_*.py`, `rewrite_*.py`, `nohup.out`,
  `update_cargo.py`, `generate_*.py`) — NOT yours; do not touch, do not
  "clean up", do not delete.
- **Doctrine**: AGENTS.md here is the same Vetcoders guide. NOTE: the
  loctree feedback channel in THIS repo is repo-local
  `.loctree/loctree-fail.md` (append-only), not `~/.vibecrafted/...`.
- Grade blast radius before changing behavior. Say so explicitly when
  verification cannot be run.

## 2. Methodology that produced loctree 0.14.5

### 2.1 Map before text (loctree-first)

Name the QUESTION, not the pattern. Real session data: two 300+-match grep
dumps produced one fact and one wrong trail; five loct calls
(`find --where-symbol` → `slice` → `body` → `occurrences` → `find` twin)
produced the module, the orbit, the single callsite (the edit plan), and
the cross-crate twin. `roles:` + `accounting:` lines are a CLOSED set —
grep head-truncation is an open set, you never know what you didn't see.
Watch scoped-laundering: scoping grep to a 200+-file dir is still
repo-mapping in disguise; the hook passes it, the doctrine doesn't.

### 2.2 Integration branch mechanics (exact protocol)

1. `gh pr list` → fetch every PR head:
   `git fetch origin refs/pull/<n>/head:pr/<n>` (worktrees may carry a
   narrow refspec — direct ref fetch always works).
2. Base = the most-ahead / most-current PR (verify with compare + the
   unique-content test below), not necessarily main.
3. Merge PRs one by one. Resolve conflicts LINE-LEVEL — never take "ours"
   wholesale, never drop the other side's intent, never discard anyone's
   work (Monika's PRs explicitly included). When a change is genuinely
   superseded, say why in the commit message.
4. Verify ancestry after each merge (`git merge-base --is-ancestor`).
5. **Review-skarby protocol** (the Founder's rule: reviews are paid-for
   specialist work, we resolve them, we never throw them away):
   - List every review thread on every PR being merged (GraphQL
     `reviewThreads { isResolved }` — batched query per PR set).
   - Fix every finding IN CODE on the integration branch; one curation
     commit is fine (reference: `90238c6` in loctree).
   - Reply to each thread with the exact fix pointer (commit + what
     changed), then resolve it. GitHub-resolve without a code fix is fraud.
   - NEVER close a PR with unresolved threads. When the mega-PR merges,
     close sub-PRs "superseded by #X" only after their threads are resolved.
6. **Provenance on every commit**:
   `[<agent>/<workflow>] type(scope): subject` +
   `Authored-By: <agent> <agents@vetcoders.io>` + `session_id:` + `time:`
   (ISO UTC) + `runtime:`. Amend script-generated stamps when they carry
   the wrong agent (`make version` auto-commits as `[codex/interactive]`
   by default — amend before push; the Founder watches this).
7. **ZAWSZE drogą PR** — nothing reaches main except via PR (version bumps
   included). Squash where the ruleset says squash.
8. Merge mechanics: check `gh api repos/<org>/<repo>/rules/branches/main`.
   `required_review_thread_resolution: true` blocks merge (BLOCKED);
   UNSTABLE = mergeable with red non-required checks. After merging,
   delete remote branches ONLY after the unique-content check:
   files changed by branch since merge-base ∩ files differing from main —
   empty intersection means nothing unique. (Squash merges make every
   branch look "ahead" by commits — ahead_by is useless; tree-diff is
   truth.)

### 2.3 CI / CodeQL reality (learned the hard way)

- Org-level "GitHub recommended" security configuration can force CodeQL
  default setup → advanced workflow SARIF uploads get rejected repo-wide
  ("CodeQL analyses from advanced configurations cannot be processed when
  the default setup is enabled"). Symptoms: GET default-setup says
  `configured`, PATCH → 422 org-controlled, DELETE → 404. Fix (keeps all
  other protections): `gh api orgs/<org>/code-security/configurations` →
  clone the configuration minus `code_scanning_default_setup`
  (`POST .../configurations`, then `POST .../{id}/attach` with
  `selected_repository_ids`), rerun failed jobs.
- CodeQL 2.27: Rust needs `build-mode: none` (autobuild refused outright);
  java-kotlin `manual` build only works where the build system actually
  lives (gradlew in a subdir, not repo root).
- Flaky process-teardown tests: bounded `yield_now` polls with generous
  deadlines (5 s proved marginal on loaded CI runners; 30 s fixed it).
  No-sleep-in-tests contract stands.
- `cargo fmt` before EVERY push — the fmt gate fails first, always.
- `gh run rerun <id> --failed` after fixing external configuration.

### 2.4 Release sequence (the actual 0.14.5 flow)

1. Integration PR merged (squash) → release branch off main →
   `make version VERSION=X.Y.Z` → **check the auto-commit stamp, amend
   agent if wrong** → push → PR → CI green → merge (squash).
2. `git tag -a vX.Y.Z -m "Release X.Y.Z ..."` — EXACT version, NO
   prerelease suffix: the tag trigger pattern `v[0-9]+.[0-9]+.[0-9]+`
   silently rejects `rc`/`dev` tags and nothing will fire.
3. `git push origin vX.Y.Z` → cascade: release-bundles → release repo →
   `publish.yml` (workflow_dispatch with the tag): crates.io → npm OIDC →
   thin repos → monorepo release → homebrew. NOTE: `release: published`
   does NOT fire from releases created by GITHUB_TOKEN — dispatch
   `homebrew-release.yml` manually with the version (designed path).
4. **npm trusted publishing**: token lane is dead by doctrine
   (PUBLISHING.md: no NPM_TOKEN fallback). Every package identity needs
   npmjs.com → package → Settings → Trusted Publisher → GitHub Actions:
   owner, repo, workflow filename (exact, with `.yml`), environment empty,
   and CHECK "Allow npm publish" (direct) — stage-only breaks a classic
   `npm publish` pipeline. E404 on PUT = missing/mismatched publisher.
   `publish-if-missing` can make a broken OIDC look green (0.14.4: all
   seven "already published" skips — the green run published NOTHING).
5. **Cold-path verification (VERIFICATION_RULE — walk around the truck)**:
   - Install from the PUBLISHED artifact, never the local build:
     `npm install -g @scope/pkg@X.Y.Z` → run the real binary → real scan
     in a cold cache dir.
   - Acid test with the EXACT stranger command in a clean HOME
     (`HOME=/tmp/x sh -c 'curl -fsSL https://<site>/install.sh | sh'`).
     This session it caught: missing Caddy redirect route (404 on
     tarballs) and missing GPG signatures (installer strict mode abort).
   - GPG: release key `8868139E8A9A2291D067135FB979B60C7079E4D4` lives on
     this machine. Verify downloaded bytes against the published
     SHA256SUMS FIRST, then sign; `gpg --detach-sign --armor` writes
     `.asc` — rename to `.sig`; upload via `gh release upload <tag> *.sig`
     (the `gh api .../assets` POST 404s; use the dedicated command).
6. **Honest release report** (4 sections): security gate (semgrep +
   findings), exposed surface inventory, deployment mode decision +
   evidence table per channel, post-release install smoke (cold path,
   artifact source named). State explicitly what is NOT verified.

### 2.5 Environment facts (this host)

- loctree-first hook blocks shell grep/rg on the working repo; pipe
  filters (`cmd | grep`) are fine; Read known files directly.
- `ssh libraxis-vm` works (alias in `~/.ssh/config`), sudo passwordless.
  `/srv/loctree-releases` is append-only; backup-before-edit convention
  (`.bak-pre-<ver>`); files are root-owned — scp to /tmp + `sudo mv`.
- Caddy: `sudo caddy validate --config /etc/caddy/Caddyfile` BEFORE
  `sudo systemctl reload caddy`; reloads in FOREGROUND only (runbook
  lesson: a reload can time out while succeeding); `redir` evaluates
  before `handle_path` in default directive order; `path_regexp` named
  groups work in redir targets (`{re.<name>.<group>}`).
- crates.io API requires a User-Agent header.
- npm trusted publishers are UI-only (no CLI). Open all identities at
  once: `open "https://www.npmjs.com/package/<name>/access" ...`.
- After OIDC is proven: harden each package with "Require two-factor
  authentication and disallow bypass 2fa tokens" (kills the token lane
  permanently — only AFTER the OIDC publish actually succeeded).

## 3. Task plans

### Task 1 — `make npm-install` for aicx

**Reuse, do not rebuild.** loctree's `distribution/npm/install-local.sh`
was written universal, its header says so: *"Reuse in another repo (e.g.
AICX): keep the same distribution/npm/<wrapper>/ layout with
platform-packages/<key>/ inside and copy this script next to it, or pass
--wrapper explicitly."* Templates to mirror: loctree `Makefile`
`npm-install`/`install-npm` (lines ~232-238), `release-binaries`
(~47-72), `distribution/npm/loct/package.json` +
`platform-packages/<key>/package.json`.

Steps:
1. Identify aicx binaries (`Cargo.toml` workspace `[[bin]]` / `loct focus
   crates`) — expected: `aicx`, `aicx-mcp`.
2. Create `distribution/npm/aicx/` with wrapper `package.json` (bin
   entries for both binaries, `files[]`, `optionalDependencies` pinning
   the four platform packages) and
   `platform-packages/{darwin-arm64,darwin-x64,linux-x64-gnu,win32-x64-msvc}/package.json`
   (bin/* in files[], version synced).
3. Makefile: `release-binaries` (STAGING_DIR, `cargo build --locked
   --release --bin aicx --bin aicx-mcp`), `npm-install` +
   `install-npm` alias calling install-local.sh with
   `NPM_LEGACY_NAMES` as appropriate. Check `build.rs` for protoc or
   other system deps and mirror the setup target if needed.
4. **Acceptance**: `make npm-install` on this host installs `aicx` +
   `aicx-mcp` into `$(npm prefix -g)/bin`, binary `--version` smoke
   passes per binary, PATH-shadow warnings surface. NOTE: old symlinks
   exist at `/usr/local/bin/aicx{,-mcp}` → `../lib/node_modules/@loctree/aicx/...`
   (an older npm prefix). Report them in the PR; do not delete without
   asking (loctree cleanup precedent: ask first).
5. PR per doctrine, CI green (`cargo fmt` first).

### Task 2 — comprehensive release, loctree-grade flow

1. Inventory the release infra: `.github/workflows/` — does aicx have
   release-bundles/publish equivalents? PR **#76** (`ci/release-npm-oidc`
   — hosted signing + npm publish via OIDC) is one of the open PRs and is
   part of this track: integrate it in task 3, exercise it here.
2. Current version: workspace `Cargo.toml` + `CHANGELOG.md` (repo has
   both). If `make version` doesn't exist, add it mirroring loctree's
   `scripts/sync-version.sh` + Makefile target — that IS part of "flow as
   pleasant and coherent".
3. Apply §2.4 verbatim: green CI → integration PR (task 3/4) → version
   PR → tag `vX.Y.Z` exact → push → cascade.
4. **Acceptance**: tag pushed; cascade green end-to-end (or each gap
   named with owner); cold-path smoke passes (`npm i -g @loctree/aicx@<ver>`
   + `aicx --version` + one real extract); 4-section release report.

### Task 3 — open PRs → one integration branch

Open at handoff time: **#72** (overlay cache perf), **#73** (cursor
first-class agent), **#75** (coverage reporting), **#76**
(release-npm-oidc), **#77** (shouted headers fix), **#78** (extract
--file default output), **#79** (pre-push delete fast-path), **#80**
(rmcp bump), **#81** (index single command — checked-out branch, likely
the base; verify ahead-ness first). Re-list at execution time.

Rules (§2.2 verbatim): fetch all heads; base = most-ahead; merge one by
one; line-level conflicts; keep everyone's work (Monika's PRs
explicitly); curation commit resolving every review thread on every
merged PR (batch the GraphQL reviewThreads query across PRs); reply +
resolve; provenance; then squash-merge the integration PR; close sub-PRs
as superseded only after their threads are resolved; delete branches
only after the unique-content test.
**Acceptance**: every PR merged or explicitly parked with a stated
reason; zero unresolved threads anywhere; CI green on the integration
branch.

### Task 4 — unmerged worktrees → integration branch → PR to main

1. `git worktree list` in this repo; also scan
   `~/.vibecrafted/worktrees/**` and `/Volumes/vc-workspace/**` for aicx
   checkouts.
2. Per worktree: branch name, ahead/behind vs main, unique-content test
   (§2.2 step 8). Merge valuable work into the task-3 integration branch;
   park garbage with a one-line reason (do not delete).
3. Then open the PR from the integration branch to main (per the task:
   PR after the worktree pull-in).
**Acceptance**: every worktree classified (merged / parked-with-reason);
integration PR to main open with full provenance and the review-thread
state clean.

### Task 5 — release surface → npm publish inclusive

1. npm state: `npm view @loctree/aicx version` (+ check for an
   `aicx-mcp` identity). Binaries `aicx` + `aicx-mcp` already exist on
   npm under `@loctree/aicx` (this host had them from an older prefix).
2. Trusted publishers (Founder's click — prep the tabs):
   `open "https://www.npmjs.com/package/@loctree/aicx/access"` (+ any
   other identity). Values: owner `Loctree`, repo `aicx`, workflow
   filename = the repo's publish workflow (after task 3 integrates #76 —
   use the exact filename), environment empty, **Allow npm publish ON**.
3. Drive the publish lane from the integrated workflow; watch each
   identity; E404 = missing publisher (§2.4.4).
4. **Acceptance**: `npm view` shows the new version on every identity;
   cold install `npm install -g @loctree/aicx@<ver>` runs and reports
   the right version; the release report's deployment table includes npm.

## 4. Standing rules (non-negotiables)

- ZAWSZE drogą PR; nothing direct to main (version bumps included).
- Provenance on every commit; amend wrong agent stamps before push.
- Never delete branches/worktrees without the unique-content check;
  never discard anyone's work — resolve, don't reject.
- Review threads: fix in code, reply with pointer, then resolve on
  GitHub. Never close with unresolved threads.
- loctree-first: name the question; grep only as a local magnifier;
  append gaps to `.loctree/loctree-fail.md` (repo-local here).
- Verification: cold path or it didn't happen; behavior over status
  codes; never trust upstream verification you didn't re-run.
- Report honestly: what was verified, what wasn't, what's parked and why.

_𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 The LibraxisAI Team_
