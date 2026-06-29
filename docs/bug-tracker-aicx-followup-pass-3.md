# bug-tracker-aicx — Follow-up Pass 3

**Source.**
- Deep review of PR #5 (fix/bug-tracker-2nd-pass @ `cbf021e`) authored
  2026-05-21 by Claude: `~/AI_notes/projects/aicx/reports/2026-05-21_pr5-bug-tracker-pass2-deep-review_claude.md`
  — 48 findings (2 P0 / 12 P1 / 23 P2 / 11 P3) across security, regression,
  quality, and hygiene axes.
- Pass-2 close-out artifacts: `~/.vibecrafted/artifacts/Loctree/aicx/2026_0521/bugtracker-aicx-pass-2/STOP-POINT-HANDOFF.md`
  — 3 explicit follow-ups (I-1 BufReader fix-impl, D2 Layer 1 reconcile,
  A13 Fleet auto-push hardening).
- Pass-1 final summary (referenced for cross-context): `~/AI_notes/projects/aicx/reports/2026-05-20_all-findings-final-session-summary.md`.

**Plan baseline.** Branch state after PR #5 merges (post `d2c30aa`), or
`origin/fix/bug-tracker-2nd-pass @ d2c30aa` if still pre-merge.

**⚠️ AUDIT FIRST.** This plan consolidates findings written 2026-05-21
just after Plan A (5 items) landed. By the time the next session runs:
- Some P1/P2/P3 may already be addressed by spontaneous follow-up commits
  (especially low-effort P3 hygiene items — ghost doc refs, CHANGELOG
  crate listing). Each worker must verify current state with `git log -S`
  and direct file reads before implementing.
- Pass-3 PR is expected to be sizable; consider splitting per AREA into
  multiple PRs to keep review surface manageable (review raport
  finding P2-pr-scope-mismatch raised this for pass-2 already).

**What is NOT in pass-3 scope.**
- Items already closed by pass-2 (G-1..G-5, H-1..H-3, I-2, I-3, I-4-retry)
  — see `docs/bug-tracker-aicx-followup-pass-2.md` and corresponding
  BUGFIXES.md entries (2026-05-21).
- Items closed by Plan A (PR #5 CI hotfixes) — commits `cbf021e`
  (semgrep + diagnostics race), `2fb1ccf` (CSRF gate drop + CORS
  wildcard), `d2c30aa` (shlex shell escape). Review raport TODO
  checkboxes for these now show `[x]`.
- Vibecrafted runtime issues (A1 zombie wrapper, A2 gemini reliability,
  A3 cancel API, A4 ps live status, A5 report path inconsistency, A6
  shared-checkout sibling collisions, A7 heartbeat metadata, A8 PATH
  shim, A9 loctree slice() disambiguation, A11 memex MCP connect, A12
  DISPATCH.md line-range carve-out, A13 worker auto-push violation)
  — those live in `~/AI_notes/projects/vibecrafted/specs/vibecrafted-runtime-pass-1.md`.

---

# AREA J — Authentication, Cryptography, Release Surface (P1)

> Security hardening that is NOT a regression vs main but was flagged by
> deep review as gaps in the new pass-2 surface. Each item has a concrete
> fix direction and bounded scope.

## J-1 (P1) — `release-linux.yml`: SHA256 checksums for release artifacts

**Symptom.** `.github/workflows/release-linux.yml:96-117` uploads
`aicx-v${VERSION}-${TARGET}-slim-unsigned.tar.gz` (explicit "unsigned").
No `sha256sum` step, no `SHA256SUMS` artifact in the release.

**Root cause.** Pass-1 added the workflow but stopped at unsigned tarballs.

**Files involved.**
- `.github/workflows/release-linux.yml`

**Fix direction.**
- Add a step after the bundle build: `sha256sum dist/*.tar.gz > dist/SHA256SUMS`.
- Add the file to `gh release upload` so consumers can verify checksums.
- Long-term follow-up (not pass-3 scope): cosign + GitHub OIDC for
  signature verification; spec into a separate ops ticket.

**Acceptance:**
- [ ] `release-linux.yml` produces `dist/SHA256SUMS` alongside the tarballs.
- [ ] Release upload includes `SHA256SUMS`.
- [ ] Documentation in `docs/RELEASES.md` (or analogous) cites the
      sha256sum verification command for end users.

**Out of scope.** Code signing (cosign / sigstore) — separate ops ticket.

---

## J-2 (P1) — `Cross.toml`: pin cross-rs image to SHA256 digest

**Symptom.** `Cross.toml:1-5` uses `ghcr.io/cross-rs/x86_64-unknown-linux-gnu:main`
— `:main` is a moving tag. Reproducibility is zero; supply-chain
compromise of `cross-rs/main` would compromise aicx release artifacts.

**Files involved.**
- `Cross.toml`

**Fix direction.**
- Replace `:main` tag with `@sha256:<digest>` pin for the current image.
- Or, less strict: pin to a specific version tag (e.g. `:0.2.5`) and
  document the update protocol.
- Adding `dependabot` for image updates is operator-side decision.

**Acceptance:**
- [ ] `Cross.toml` image references include SHA256 digest or fixed
      version tag, not `:main`.
- [ ] Brief CHANGELOG/RELEASES note explaining the pin.

**Out of scope.** Dependabot / automated bump pipeline.

---

## J-3 (P1) — `auth.rs::generate_token`: cross-platform CSPRNG

**Symptom.** `src/auth.rs:127-133` opens `/dev/urandom` directly. Compiles
on Windows but fails at runtime (`File::open("/dev/urandom")` returns
ENOENT). Currently aicx builds for Linux/macOS (the only operating
systems `release-linux.yml` and the cross macOS workflow target), but
if Windows support is ever added the token generation is dead path.

**Files involved.**
- `src/auth.rs`

**Fix direction.**
- Replace `/dev/urandom` read with `getrandom::getrandom(&mut buf)?`
  (small, well-audited crate) or `rand::rngs::OsRng::fill_bytes(&mut buf)`
  (rand is already a transitive dep).
- Same 32-byte output, same hex-encode path.

**Acceptance:**
- [ ] `generate_token` no longer references `/dev/urandom`.
- [ ] Build + test green on macOS + Linux (CI).
- [ ] Generated token is still 64-hex-char (32 bytes / 256-bit).

**Out of scope.** Other entropy uses in the codebase — audit them
in J-3b if discovered, but `auth.rs` is the only currently-flagged site.

---

## J-4 (P1) — `auth.rs`: Windows ACL protection for token file

**Symptom.** `src/auth.rs:153-162` chmod-protects `~/.aicx/auth-token`
with mode 0600 under `#[cfg(unix)]`. No equivalent for Windows; on
Windows the token file lands with default ACL, readable by
`Authenticated Users` (or worse).

**Files involved.**
- `src/auth.rs`

**Fix direction.** Two options:
- (a) Refuse to run on Windows with a clear error: "Run aicx auth on
  Linux/macOS, or pass `--auth-token <token>` explicitly so the token
  file is never written."
- (b) Add `windows-acl` crate dep + `SetFileSecurity` call to restrict
  DACL to current SID.

Option (a) is the lean choice while Windows is not officially supported.

**Acceptance:**
- [ ] On Windows, `aicx auth` either (a) returns clear `RefusedOnWindows`
      error or (b) writes the file with restricted DACL.
- [ ] Documented in `docs/SECURITY.md` or auth-specific doc.

**Out of scope.** Linux/macOS path remains unchanged.

---

## J-5 (P1) — `auth.rs::persist_token_file`: TOCTOU between write and chmod

**Symptom.** `src/auth.rs:145-164` calls `fs::write(path, ...)` then
`set_permissions(...)` separately. Between the two syscalls the file
exists with default permissions (umask-determined, usually 0644). A
local process can read the file in that window.

**Files involved.**
- `src/auth.rs`

**Fix direction.**
- Use `OpenOptions::new().mode(0o600).create_new(true).write(true).open(path)`
  on Unix (mode is set atomically during `open()` with `O_CREAT`).
- Or write via `tempfile::NamedTempFile` + chmod-on-tempfile + atomic
  rename to target.

**Acceptance:**
- [ ] No window where the token file is readable beyond owner.
- [ ] Existing tests pass.
- [ ] Add test asserting created file has mode 0600 immediately after
      first `persist_token_file` call.

**Out of scope.** Windows path (covered by J-4).

---

## J-6 (P1) — `state.rs`: zero-byte separator in `content_hash`/`overlap_hash`

**Symptom.** `src/state.rs:351-357,366-374`:
```rust
data.extend_from_slice(agent.as_bytes());
data.extend_from_slice(&timestamp.to_le_bytes());
data.extend_from_slice(message.as_bytes());
```
Field concatenation without separator → hash splitting attack surface.
For some `(agent, ts, msg)` triples, an attacker can craft a different
triple yielding the same byte stream. Practically: not exploitable
without write access to agent/ts (the inputs that AICX accepts), but
defense-in-depth + cryptographic correctness call for separator.

**Files involved.**
- `src/state.rs`

**Fix direction.**
- Switch to `blake3::Hasher::new().update(...).update(&[0u8]).update(...)` pattern.
- Or length-prefix encoding: `data.extend_from_slice(&(agent.len() as u32).to_le_bytes())`
  before each field.

**⚠️ Migration concern.** Changing the hash function semantics will
INVALIDATE the existing `seen_hashes` cache — same `seen_hashes`-reset
treatment as G-1 BLAKE3 migration. Bump `hash_algorithm` to
`"blake3-128-v2"` and reuse the migration path. Add CHANGELOG breaking
note (cross-references K-2).

**Acceptance:**
- [ ] Separator (zero-byte or length-prefix) inserted between fields.
- [ ] `hash_algorithm` constant bumped + migration path covers it.
- [ ] CHANGELOG entry under Unreleased: "BREAKING: state dedup hash
      bumped from blake3-128-v1 to blake3-128-v2; first post-upgrade
      `aicx store` will re-process recently-seen entries (no data
      loss, possible timeline duplicates from a parallel running
      process)".
- [ ] Test demonstrating that two triples that previously collided
      no longer do.

**Out of scope.** Wholesale rewrite of state schema beyond the
algorithm bump.

---

## J-7 (P1) — `redact.rs`: GCP service-account regex quantifier limits

**Symptom.** `src/redact.rs:43`:
```rust
Regex::new(r#"(?s)\{[^{}]{0,500}"type"\s*:\s*"service_account"[^{}]{0,2500}\}"#)
```
GCP service-account JSON with extra fields, comments, or long names
exceeds these limits. `private_key` is still redacted by a separate
regex, but `private_key_id` and `client_email` only redact when the
WHOLE service-account object matches — silent leak otherwise.

**Files involved.**
- `src/redact.rs`

**Fix direction.**
- Either: raise the upper quantifier to 16_384 + add a fail-loud
  heuristic (post-redaction scan for "service_account" literal → log
  warning if found).
- Or: split into per-field standalone regexes that don't depend on
  matching the entire object.

Per-field is cleaner; raise-limit is faster.

**Acceptance:**
- [ ] Service-account JSON > 2.5 KiB still redacts `private_key_id`
      and `client_email`.
- [ ] Test fixture: large service-account JSON (synthesized 4 KiB) →
      assert all sensitive fields redacted.
- [ ] No regression on existing redaction tests.

**Out of scope.** AWS / Azure equivalent objects — separate audit if
needed.

---

# AREA K — Tests & Documentation Hygiene (P1)

## K-1 (P1) — `tests/dashboard_security.rs`: empty fake test

**Symptom.** `tests/dashboard_security.rs:1-4`:
```rust
#[test]
fn test_run_memex_cli_uses_resolved_absolute_path_not_path() {
    // Verified statically via the which::which usage in dashboard_server.rs
}
```
Empty test body. Name implies behavioural verification, actually asserts
nothing. Compliance theater.

**Files involved.**
- `tests/dashboard_security.rs`

**Fix direction.**
- Either: remove the file entirely (no test is better than a fake one).
- Or: write a real test — e.g. spawn a subprocess with PATH containing
  only a temp dir holding a `rust-memex` shim; verify the resolved path
  is absolute.

Honest fix is to write the real test; if effort > value, delete and
note in `docs/BACKLOG.md`.

**Acceptance:**
- [ ] `tests/dashboard_security.rs` either contains at least one
      meaningful assertion OR is removed.
- [ ] If removed, BACKLOG.md entry documents the gap.

**Out of scope.** Wider test-coverage audit — separate task.

---

## K-2 (P1) — CHANGELOG breaking note: siphash13 → blake3-128 migration

**Symptom.** `CHANGELOG.md` Unreleased section does NOT mention the
state-dedup-hash migration introduced by pass-2 G-1 (`0c9ba5e`).
After first `aicx store` post-upgrade, `seen_hashes` is reset →
recently-seen entries get re-processed, producing duplicates in the
extracted timeline if a parallel ingest is running.

**Files involved.**
- `CHANGELOG.md`

**Fix direction.**
- Add `### Breaking` block under Unreleased:
  ```
  ### Breaking
  
  - state.json hash algorithm migrated from `siphash13-v1` to
    `blake3-128-v1`. The dedup cache (`seen_hashes`) is cleared on
    first load after upgrade; `aicx store` will re-process the
    recent `-H` window once. No data loss, but timeline may show
    duplicates if a parallel ingest is running.
  ```
- If J-6 also lands in pass-3, mention the second bump
  (`blake3-128-v1` → `blake3-128-v2`).

**Acceptance:**
- [ ] CHANGELOG.md Unreleased contains the BREAKING note.
- [ ] Tested by a fresh-checkout run reading CHANGELOG and finding the
      migration warning before upgrading state.

**Out of scope.** Format / styling of CHANGELOG (Keep a Changelog
already in use — extend it, don't rewrite).

---

# AREA L — Pass-2 Leftovers

> Three follow-ups explicitly carried forward from pass-2
> `STOP-POINT-HANDOFF.md`.

## L-1 (P1 — pass-2 carry-over) — BufReader caps workspace-wide fix-impl

**Symptom.** Pass-2 W-D-4 (commit `26d8123`) landed an AUDIT-ONLY doc
identifying `BufReader::lines()` / `read_to_string` sites lacking
MAX_LINE_BYTES cap. Worker honoured brief protocol "split into
audit-only commit if >5 missing sites surfaced". Pass-3 closes the
actual implementation per the audit's listed sites.

**Files involved.**
- All BufReader call sites enumerated in `26d8123` audit doc
  (consult `git show 26d8123` for the per-site list).
- New regression test asserting cap behaviour per affected module.

**Fix direction.**
- Apply `MAX_LINE_BYTES` (existing constant) cap to each site OR
  document why a particular site is bounded-by-construction (e.g.
  config file with known small size).
- Group into per-module commits where possible for reviewability.

**Acceptance:**
- [ ] Every site listed in `26d8123` audit doc either capped or
      documented as out-of-scope (with rationale on the call site).
- [ ] Per-module regression test: synthetic `MAX_LINE_BYTES + 1` input
      → reader returns capped error, no OOM.
- [ ] BUGFIXES.md entry citing `26d8123` audit and the close-out SHA.

**Out of scope.** Changing `MAX_LINE_BYTES` value. Refactoring
BufReader abstraction.

---

## L-2 (operator-side smoke) — Layer 1 reconcile (`aicx store --full-rescan`)

**Symptom.** Pass-2 W-D-2 (H-2 followup verdict) determined that G-3
incremental walk addresses Layer 2 (embeddings) only, NOT Layer 1
(`~/.aicx/index.json` ⇄ canonical store). Operator's `aicx doctor`
still reports 188 orphan + 40 missing tuples. Pass-2 left this as
operator-side smoke: run `aicx store --full-rescan` against canonical
store, idempotent, ~minutes.

**Files involved.**
- None (operator runtime task, not code change).
- Result documentation: BACKLOG.md `partial(@c74deb1)` line updated
  to `done(@c74deb1 + smoke 2026-MM-DD)` or analogous post-run.

**Fix direction.**
- Operator runs: `aicx store --full-rescan` on canonical store
  (production: `$HOME/.aicx/store`).
- Inspect `aicx doctor` before / after counters.
- If counters drop to 0: update BACKLOG.md status. If non-zero
  remainder: spec a code-side reconcile task for pass-3+.

**Acceptance:**
- [ ] `aicx doctor` reports 0 orphans + 0 missing tuples post-rescan.
- [ ] BACKLOG.md entry updated.
- [ ] If gap persists: separate task documented for code fix.

**Out of scope.** Code change (unless smoke reveals new bug).

---

## L-3 (cross-repo) — Vibecrafted Fleet auto-push hardening

**Symptom.** Pass-2 finding A13: vibecrafted Fleet wrapper occasionally
auto-pushes worker commits to `origin` despite explicit `DO NOT push`
instruction in brief. Observed in pass-1 (claude W6-B `148f8b0`) and
pass-2 (codex G-2 `7ded8cb`, claude G-4 cluster pulled along on push).
Soft charter violation; branch was stable in all observed cases, so no
damage, but the AUTONOMY.md hard-stop says push requires explicit
operator button.

**Files involved.**
- NOT IN aicx REPO. Lives in vibecrafted source tree.
- Spec target: `~/AI_notes/projects/vibecrafted/specs/vibecrafted-runtime-pass-1.md`.

**Fix direction.**
- Audit Fleet wrapper for implicit `git push` step.
- Worker prompt should disable auto-push declaratively (already in brief;
  needs runtime enforcement).
- May require collaboration with vibecrafted maintainers (Maciej).

**Acceptance:**
- [ ] Vibecrafted-side issue / spec entry linked from this task.
- [ ] Cross-reference in BACKLOG.md under `[vibecrafted]` namespace.

**Out of scope.** Any aicx-side change. Tracked here only because
multiple pass-2 commits were affected and a follow-up sweep is owed
to the cross-repo backlog.

---

# AREA M — Quality, Defense-in-Depth, Code Smell (P2)

> Items rated P2 by deep review — fix-or-document. None are blockers
> for merge; address as time permits, typically in topical follow-up
> commits.

## M-1 (P2) — `auth_middleware`: length-mismatch short-circuit before constant_time_eq

**Symptom.** `src/auth.rs:170-179` comment acknowledges a known
length-channel timing leak. Token is fixed 64-hex, so the leak gives
attacker zero information today, but defensively closing it future-proofs
against token type changes.

**Files involved.** `src/auth.rs`

**Fix direction.** In `auth_middleware` add an early-return 401 when
`provided.len() != expected.len()` BEFORE invoking constant_time_eq.

**Acceptance:**
- [ ] Code path verified by inspection (constant-time-safe).
- [ ] No regression on existing auth tests.

---

## M-2 (P2) — Bearer endpoints: rate limiting / brute-force protection

**Symptom.** No `tower::limit` / `tower_governor` on
`require_auth_layer`. 64-hex bearer is not brute-forceable, but lack
of rate-limit creates SOC blindness and DoS surface.

**Files involved.** `src/auth.rs`, `src/dashboard_server.rs`,
new `Cargo.toml` dep (`tower_governor`).

**Fix direction.** Add `tower_governor::governor::GovernorLayer` to
auth layer with conservative defaults (e.g. 100 req/min per IP).

**Acceptance:**
- [ ] 401 brute-force at >100 req/min returns 429 after threshold.
- [ ] Existing valid-bearer requests not throttled at normal cadence.

---

## M-3 (P2) — `regenerate_dashboard`: enforce Origin/Referer for mutate endpoints

**Symptom.** `src/dashboard_server.rs:677-694` — Origin/Referer check
only runs if header is present. Non-browser clients (curl, scripts)
bypass it entirely. Combined with the now-dropped CSRF gate
(`2fb1ccf`), browser flow is the ONLY one even nominally protected.

**Files involved.** `src/dashboard_server.rs`

**Fix direction.** Make Origin/Referer presence mandatory for mutating
endpoints (POST/PUT/DELETE) when the request comes through the public
router. Allow tools/scripts via explicit `--allow-no-origin` config
flag if needed.

**Acceptance:**
- [ ] POST without Origin AND without Referer → 403.
- [ ] Test coverage for both presence and match cases.

**Out of scope.** GET endpoints (read-only).

---

## M-4 (P2) — `cross_search.limit`: silent clamp to 200

**Symptom.** `src/dashboard_server.rs:377`: `let limit = params.limit.min(200);`
— client request for 500 receives 200 without notification. Pagination
clients may loop without progress.

**Files involved.** `src/dashboard_server.rs`

**Fix direction.** Either: reject with 400 when `limit > 200`, OR
return `X-Clamped-Limit: 200` response header.

**Acceptance:**
- [ ] Client receives signal that the limit was clamped.

---

## M-5 (P2) — Generic 403 error for CSRF / action-header failure

**Symptom.** `src/dashboard_server.rs:660-664` returns error body
referencing exact header names. Reveals implementation surface to
attackers.

**Files involved.** `src/dashboard_server.rs`

**Fix direction.** Replace specific error with generic
`{"ok": false, "error": "Forbidden"}` body. Server logs can still
record the specific reason.

**Acceptance:**
- [ ] 403 body is opaque to clients.
- [ ] Server log records detailed reason (`tracing::warn!`).

---

## M-6 (P2) — `is_under_allowed_base`: macOS `/Users/{x}/{y}/...` is too broad

**Symptom.** `crates/aicx-parser/src/sanitize.rs:107-112` allows ANY
`/Users/{x}/{y}/...` with 3+ components. A malicious agent could request
`validate_read_path("/Users/user/Documents/secret.txt")` and it
passes the allowlist. Filesystem permissions usually save us, but
a shared or world-readable directory leaks.

**Files involved.** `crates/aicx-parser/src/sanitize.rs`

**Fix direction.** Restrict to `dirs::home_dir()` of the current user
+ `dirs::cache_dir()` + `dirs::data_dir()`. Don't generalize over `/Users`.

**Acceptance:**
- [ ] `/Users/user/...` rejected by validate_read_path on macOS.
- [ ] Existing tests pass.

---

## M-7 (P2) — `AICX_ALLOW_TMP`: unify debug + release policy

**Symptom.** `crates/aicx-parser/src/sanitize.rs:86-90` allows /tmp in
test + non-release builds (debug) by default; release requires
`AICX_ALLOW_TMP=1`. Dev workstations have more secrets in /tmp than
prod — operator could argue parity is better.

**Files involved.** `crates/aicx-parser/src/sanitize.rs`

**Fix direction.** Require `AICX_ALLOW_TMP=1` for non-test builds
(both debug AND release). Keep `cfg(test)` auto-allow for cargo test.

**⚠️ Trade-off.** Forces dev workflow to export the env var. May annoy
contributors who fire ad-hoc `cargo run` against /tmp fixtures.

**Acceptance:**
- [ ] Debug build without env var rejects /tmp paths.
- [ ] Test build (cargo test) still allows /tmp without env var.
- [ ] CONTRIBUTING.md notes the env var requirement for dev runs.

---

## M-8 (P2) — `RetrieveError::GenerationMismatch`: per-field error variants

**Symptom.** `crates/aicx-retrieve/src/orchestrator.rs:209-212` reuses
`lexical_gen`/`dense_gen` fields to carry non-pairing data (lexical_gen
field receives dense_count). Debugging mismatch will be misleading.

**Files involved.** `crates/aicx-retrieve/src/orchestrator.rs`

**Fix direction.** Add dedicated variants:
`DenseCountMismatch { expected, actual }`,
`LexicalDocCountMismatch`,
`LexicalCommitMismatch`.

**Acceptance:**
- [ ] Each mismatch kind has its own variant with named fields.
- [ ] Existing callers of `GenerationMismatch` updated.

---

## M-9 (P2) — `Builder::commit()`: clean borrow instead of `.expect`

**Symptom.** `crates/aicx-retrieve/src/orchestrator.rs:130-137` uses
`.expect("manifest checked above")` after `is_some()` check. Logically
sound, but code smell.

**Files involved.** `crates/aicx-retrieve/src/orchestrator.rs`

**Fix direction.**
```rust
let manifest = self.manifest.as_ref()
    .ok_or_else(|| anyhow!("cannot commit hybrid index before build_hybrid"))?;
```

**Acceptance:**
- [ ] No `.expect` in `commit()`.
- [ ] Error path tested.

---

## M-10 (P2) — `Mutex.lock().unwrap()` in `mcp.rs` hot path

**Symptom.** `src/mcp.rs:597,611` — embedder_unavailable_until accessed
via `.lock().unwrap()`. Poisoned mutex (rare but possible after a panic
in another handler) crashes the async task.

**Files involved.** `src/mcp.rs`

**Fix direction.** Either `.expect("embedder_unavailable_until poisoned; programmer bug")`
with descriptive message, or recover from poison via `unwrap_or_else(|e| e.into_inner())`
and reset state to safe default.

**Acceptance:**
- [ ] No `.unwrap()` on mutex lock in this hot path.
- [ ] Recovery path covered by test (synthesize panic, verify next
      lock acquisition succeeds).

---

## M-11 (P2) — `serde_json::to_string(...).unwrap()` in `aicx_steer` MCP handler

**Symptom.** `src/mcp.rs:1109` — score may be `f32::NAN` → JSON
serialization fail → panic in MCP handler.

**Files involved.** `src/mcp.rs`

**Fix direction.** Propagate via `?` and return structured error
response.

**Acceptance:**
- [ ] No `.unwrap()` on `to_string` in this handler.
- [ ] Test: synthesize NaN score input → handler returns error JSON
      instead of crashing.

---

## M-12 (P2) — `write_conversation_*` not redacting by default

**Symptom.** `tests/secret_redaction_e2e.rs:98` shows caller is
expected to redact manually. `src/output.rs` `write_conversation_*`
does NOT call `redact_secrets`. External integration or MCP tool
calling these with raw `ConversationMessage` → secrets leak.

**Files involved.** `src/output.rs`, possibly `src/main.rs` callers.

**Fix direction.** Two options:
- (a) `write_conversation_*` redact by default, opt-out via explicit
  flag.
- (b) `ConversationMessage::new_redacted(...)` constructor enforced
  by API shape.

Option (a) is the lean choice (fewer call-site changes).

**Acceptance:**
- [ ] Default `write_conversation_*` produces redacted output.
- [ ] Opt-out path tested.
- [ ] Regression test: raw token in message → not in output.

---

## M-13 (P2) — CSP: drop `unsafe-inline`, use nonces

**Symptom.** `src/dashboard.rs:267` CSP header includes
`'unsafe-inline'` for script-src and style-src. Negates much of CSP's
XSS protection.

**Files involved.** `src/dashboard.rs`

**Fix direction.** Replace `'unsafe-inline'` with `'nonce-<random>'`.
Inject nonce in response header AND on every inline `<script>` /
`<style>` element in shell_html. Renderer rework required.

**⚠️ Effort.** Touches HTML renderer; non-trivial.

**Acceptance:**
- [ ] CSP header no longer contains `'unsafe-inline'`.
- [ ] All inline script/style elements carry matching nonces.
- [ ] Headless browser test verifies no CSP violation in console.

**Out of scope.** Moving to external script files (separate refactor).

---

## M-14 (P2) — `tests/dashboard.rs:test_inline_markdown_*`: marker-literal tests

**Symptom.** `src/dashboard.rs:2670-2683` tests assert presence of
literal strings (e.g. `lower.startsWith('javascript:')`) in embedded
JS, NOT actual behaviour. Refactor of JS source = test break despite
identical behaviour.

**Files involved.** `tests/dashboard.rs`, possibly new dependency
(`wasm-bindgen-test` or playwright/puppeteer).

**Fix direction.** Either:
- (a) Extract `inlineMarkdown` to pure JS module, test via Node.
- (b) Use headless browser test (heavier infra).

Pick (a) for lean delivery.

**Acceptance:**
- [ ] Tests assert behaviour (XSS-blocked schemes don't render), not
      literal strings.
- [ ] Refactor of JS internals doesn't break tests.

---

## M-15 (P2) — `atomic_write` tempfile naming: atomic counter for collisions

**Symptom.** `src/store/atomic_write.rs:51-57` uses only
`subsec_nanos` for uniqueness. High-QPS multi-thread scenarios can
collide → corrupted commit.

**Files involved.** `src/store/atomic_write.rs`

**Fix direction.** Add `static COUNTER: AtomicU64 = AtomicU64::new(0)`
appended to tempfile basename. Provides true uniqueness regardless
of nanosec resolution.

**Acceptance:**
- [ ] Stress test: 100 concurrent writes → no collisions, no
      corrupted commits.

---

## M-16 (P2) — Lock holder sidecar: extend to all lockfiles

**Symptom.** `src/locks.rs:229-235` writes holder sidecar only for
`lance.lock` (exclusive). state.lock, mcp.lock, etc. don't get
PID/run_kind info — incident triage is harder.

**Files involved.** `src/locks.rs`

**Fix direction.** Default sidecar generation to all lockfiles
(opt-out per lock if performance matters). Same shape as G-2 sidecar.

**Acceptance:**
- [ ] Every lock acquire writes corresponding `.holder` sidecar.
- [ ] Existing G-2 lance.lock behaviour unchanged.
- [ ] sidecar cleanup on release works for all locks.

---

## M-17 (P2) — State load: triple JSON parse pass

**Symptom.** `src/state.rs:224,231,243` parses `contents` with
`serde_json::from_str` three times (strict, value, legacy). Performance
overhead for state.json > 1 MB.

**Files involved.** `src/state.rs`

**Fix direction.** Parse to `serde_json::Value` once, then use
`serde_json::from_value::<Strict>` / `from_value::<Legacy>` on the
parsed Value (avoid re-parsing string).

**Acceptance:**
- [ ] Single parse for the strict + legacy paths.
- [ ] Existing migration tests pass.
- [ ] Measure improvement on 1+ MB state.json (informational).

---

## M-18 (P2) — Self-healing: after backup recovery, save to primary

**Symptom.** `src/state.rs:163-196` `load_from_path_with_legacy_warning`
returns recovered state from backup BUT does NOT save to primary path.
Next load goes through backup-recovery path again. Self-healing is
incomplete.

**Files involved.** `src/state.rs`

**Fix direction.** After successful backup recovery, schedule
`save_to_path(primary_path)` (synchronously or in background) so
corruption doesn't persist.

**Acceptance:**
- [ ] After backup recovery, primary path is freshly written with
      recovered state.
- [ ] Test demonstrating: corrupt primary → load → primary auto-fixed.

---

## M-19 (P2) — Update `lru` (RUSTSEC-2026-0002) + replace `paste` (RUSTSEC-2024-0436)

**Symptom.** `cargo audit` warnings:
- `lru < 0.16.3`: unsound IterMut / Stacked Borrows violation under Miri.
- `paste`: unmaintained.
- RSA Marvin Attack in transitive deps (separate; out of pass-3 scope unless
  hot-path usage discovered).

**Files involved.** `Cargo.toml`, `Cargo.lock`.

**Fix direction.**
- `cargo update -p lru` to >= 0.16.3.
- `cargo update -p paste` to latest, or replace with `pastey` (drop-in).
- RSA Marvin Attack: audit usage; if transitive-only and not in
  cryptographic hot path, add `[ignore]` to `cargo-audit.toml` with
  rationale, plus a follow-up task to track upstream fix.

**Acceptance:**
- [ ] `cargo audit` returns 0 advisories for `lru` and `paste`.
- [ ] RSA: either updated, replaced, or ignored with documented rationale.

---

## M-20 (P2) — `retrieval-eval.yml`: concurrency cancel-in-progress

**Symptom.** `.github/workflows/retrieval-eval.yml` has no `concurrency`
section. Each push to a PR triggers a fresh run; older runs aren't
cancelled. Wastes CI minutes.

**Files involved.** `.github/workflows/retrieval-eval.yml`

**Fix direction.**
```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number }}
  cancel-in-progress: true
```

**Acceptance:**
- [ ] Multiple rapid pushes cancel previous runs.

---

## M-21 (P2) — `actions/checkout` version drift between workflows

**Symptom.** `release-linux.yml:36` uses `@v6`, `retrieval-eval.yml:14`
uses `@v4`. Cosmetic but audit-friendly.

**Files involved.** All `.github/workflows/*.yml`.

**Fix direction.** Align to a single major (`@v6` is current); update
`retrieval-eval.yml`.

**Acceptance:**
- [ ] All workflows use the same `actions/checkout` major version.

---

## M-22 (P2) — `env_clear()` for `run_memex_cli`: passthrough HOME/XDG_* if needed

**Symptom.** `src/dashboard_server.rs:339` uses `Command::env_clear()` for
memex CLI invocation. Good isolation, but binaries that need `HOME` /
`XDG_*` for config-dir resolution may fail.

**Files involved.** `src/dashboard_server.rs`, `docs/`.

**Fix direction.**
- Test: actually run `rust-memex` / `rmcp-memex` with env-less env in
  CI/dev environment; confirm they work.
- If they need HOME/XDG: passthrough explicitly with `Command::env(...)`
  for those specific vars only.

**Acceptance:**
- [ ] CI run that invokes memex CLI succeeds (either env_clear is fine
      OR explicit passthrough added).
- [ ] Document the contract in `docs/` (memex CLI must be
      self-contained OR aicx passes HOME/XDG).

---

## M-23 (P2) — PR scope mismatch: rebrand or split

**Symptom.** PR #5 title and branch name (`fix/bug-tracker-2nd-pass`)
suggest tactical bug fix; actual scope is feature work (3 new crates,
+20k/-3k LOC, 157 new public APIs). Review surface is hard to manage.

**Files involved.** PR-level — operator action.

**Fix direction.**
- Either: rewrite PR #5 title + description to reflect actual scope
  ("bug-tracker pass-2: closures + feature rollout: hybrid retrieve,
  diagnostics, monitor, progress-contracts").
- Or: split future PRs by AREA (e.g. Area J = one PR, Area K = another).

**Acceptance:**
- [ ] If keeping PR #5 as-is: title + description reflect actual scope.
- [ ] If splitting future work: pass-3 PR boundaries declared at the
      top of this plan.

**Out of scope.** Splitting the already-pushed PR #5 (too costly).

---

# AREA N — Hygiene (P3)

> Low-priority; can land post-merge or in batched cleanup.

## N-1 (P3) — Ghost doc references to `src/sanitize.rs` and `src/chunker.rs`

**Symptom.** Multiple docs reference `src/sanitize.rs` / `src/chunker.rs`,
but these files now live under `crates/aicx-parser/src/`. Re-export via
`src/lib.rs:42` keeps code compiling; doc rot remains.

**Files involved.**
- `docs/ARCHITECTURE.md:40,43,133`
- `crates/aicx-parser/CHANGELOG.md:35`
- `crates/aicx-parser/README.md:56,73-75`
- `reports/NOISE_FILTER_v0.6.3.md:77,263`

**Fix direction.**
- `grep -rl "src/sanitize\|src/chunker" docs/ crates/aicx-parser/ reports/`.
- Replace with correct paths or remove the reference.

**Acceptance:**
- [ ] `grep` shows zero matches for ghost paths.

---

## N-2 (P3) — `run_memex_cli`: hardcoded 30s timeout

**Symptom.** `src/dashboard_server.rs:345` uses `Duration::from_secs(30)`
literal. Should be configurable.

**Files involved.** `src/dashboard_server.rs`, `DashboardServerConfig`.

**Fix direction.** Add `memex_timeout_secs: u64` field to
`DashboardServerConfig` with default 30.

**Acceptance:**
- [ ] Config field exists with default 30.
- [ ] CLI flag or env var to override.

---

## N-3 (P3) — `parse_relative_time` saturating arithmetic — add explanatory comment

**Symptom.** `src/dashboard_server.rs:236` uses `saturating_sub` /
`saturating_mul`. Good defensive style, but a one-line comment would
spare the reader from inferring intent.

**Files involved.** `src/dashboard_server.rs`.

**Fix direction.** Add `// saturate on overflow rather than panic; user
input may be adversarial` above the line.

**Acceptance:**
- [ ] Comment present.

---

## N-4 (P3) — `looks_like_date_dir` heuristic — bare doc-comment

**Symptom.** `src/state/migration.rs:180-187` uses a 9-char + underscore-at-5
+ exactly-8-digits heuristic. Niche corner cases may pass / fail.

**Files involved.** `src/state/migration.rs`

**Fix direction.** Add doc comment describing the heuristic shape
(`2026_0521` style) + listed edge cases. Optionally widen to true
date validation via `chrono::NaiveDate::parse_from_str`.

**Acceptance:**
- [ ] Doc comment present.
- [ ] (Optional) chrono-based parse with fallback to heuristic.

---

## N-5 (P3) — `looks_like_date_dir` reuse in `is_probably_repo_name`: undocumented dispersion

**Symptom.** `looks_like_date_dir` is reused implicitly by
`is_probably_repo_name`. Mentioned in review but not detailed; appears
to be a tracking item to confirm semantics match across the two
predicates.

**Files involved.** `src/state/migration.rs`

**Fix direction.** Confirm both callers' assumptions are aligned;
extract to a shared private helper with a single doc-comment + test.

**Acceptance:**
- [ ] Single source of truth for the heuristic.
- [ ] Both callers documented to share semantics.

---

## N-6 (P3) — CHANGELOG: add entries for new crates (aicx-monitor, aicx-progress-contracts, aicx-retrieve)

**Symptom.** `CHANGELOG.md` Unreleased section does not list the three
new crates introduced by pass-2 feature expansion.

**Files involved.** `CHANGELOG.md`.

**Fix direction.** Add Added/Internal entries for each crate with brief
description.

**Acceptance:**
- [ ] CHANGELOG mentions each new crate.

---

## N-7 (P3) — `RE_JWT` regex permissive

**Symptom.** `src/redact.rs:31-32` matches `eyJ...eyJ...` patterns
permissively. Some non-JWT 30+ char strings may match.

**Files involved.** `src/redact.rs`.

**Fix direction.** Tighten to typical JWT shape (alphabet + length
ranges per segment). Add test fixtures: legitimate non-JWT strings
that previously got false-positive-redacted.

**Acceptance:**
- [ ] Reduced false-positive rate on a curated non-JWT corpus.
- [ ] Existing real-JWT redaction tests pass.

---

## N-8 (P3) — Add Stripe webhook secret (`whsec_*`) to `redact.rs`

**Symptom.** `redact.rs` does not match `whsec_*` (Stripe webhook
signing secret).

**Files involved.** `src/redact.rs`.

**Fix direction.** Add `\bwhsec_[A-Za-z0-9]{20,}\b` regex to the
redact list.

**Acceptance:**
- [ ] Stripe webhook secrets redacted on output.

---

## N-9 (P3) — `DEFAULT_TIMEOUT` lock change documented

**Symptom.** `src/locks.rs:21` increased default timeout 5s → 60s in
pass-2. Trade-off (fewer false-fail on slow ops vs longer freeze on
stuck lock) is subjective.

**Files involved.** `src/locks.rs`, possibly `docs/`.

**Fix direction.** Doc comment on the constant explaining rationale;
no code change needed.

**Acceptance:**
- [ ] Doc comment present.

---

## N-10 (P3) — `cargo audit` "Cargo audit" gate classification

**Symptom.** `MERGE_GATE.json` classifies `cargo audit` failure as
"pre-existing quality failure". Marvin Attack RSA was in deps before
this PR. CHANGELOG could mention.

**Files involved.** `CHANGELOG.md`.

**Fix direction.** Add note under `Known Issues` referencing the
RSA Marvin Attack advisory + transitive-only status.

**Acceptance:**
- [ ] CHANGELOG mentions known issue.

---

## N-11 (P3) — `cargo geiger` timeout (600s) — toolchain issue

**Symptom.** `cargo geiger` times out at 600s on this workspace.

**Files involved.** Tooling — likely prview-rs side.

**Fix direction.** Out of aicx scope. Track upstream in prview-rs
issues.

**Acceptance:**
- [ ] Upstream issue linked in BACKLOG.md `[prview]` namespace.

---

## N-12 (P3) — `BLAKE3-128` truncation: use byte slice + hex-encode, not hex-prefix

**Symptom.** `src/state/migration.rs:33-38`:
```rust
let hex = hash.to_hex();
hex[..32].to_string()
```
Functional + safe (prefix preservation), but stylistically should use
`&hash.as_bytes()[..16]` + `hex::encode` (or `hex::encode_to_slice`)
to make truncation-point obvious.

**Files involved.** `src/state/migration.rs`

**Fix direction.** Rewrite as bytes-truncation. Same output.

**Acceptance:**
- [ ] Cleaner truncation pattern.
- [ ] Tests pass (hash output is identical).

---

# Suggested wave grouping for pass-3 dispatch

```text
Wave A (foundation — pass-2 leftover, single worker):
  L-1 BufReader caps workspace fix-impl (audit at 26d8123)

Wave B (security parallel — file-disjoint):
  J-1 release SHA256 (workflow YAML only)
  J-2 Cross.toml pin (config only)
  J-3 auth getrandom (auth.rs)
  J-7 GCP redact regex (redact.rs)

Wave C (security sequential — same auth.rs file):
  J-4 Windows ACL (refuse-on-Windows)
  J-5 TOCTOU mode-on-create

Wave D (state hash migration — requires CHANGELOG coordination):
  J-6 hash separator + blake3-128-v2 bump + CHANGELOG breaking note (K-2)

Wave E (tests + docs):
  K-1 fake test removed or real
  N-6 changelog crates listing
  N-1 ghost doc refs cleanup
  N-12 blake3 truncation style

Wave F (operator-side smoke, not code):
  L-2 aicx store --full-rescan against canonical store

Wave G (cross-repo, not aicx):
  L-3 Vibecrafted A13 spec entry

Wave H (P2 batch — group by area, ~3-4 commits):
  M-1..M-6 auth/dashboard hardening
  M-7..M-13 quality/code-smell sweep
  M-14..M-18 tests/state polish
  M-19..M-22 deps + CI workflow polish
  M-23 PR-scope (operator action)

Wave I (P3 hygiene batch — single docs commit):
  N-2..N-5, N-7..N-11
```

**Agent rotation for pass-3** (per AGENT FAIRNESS, balanced with pass-2
totals):
- claude × N · codex × N · gemini × N (gemini may rotate back in if
  upstream `Failed to edit` loop fix lands — patrz vibecrafted-runtime-pass-1
  A-2 status).

**Cross-task sibling warnings:**
- J-4 + J-5 share `src/auth.rs`. Sequential, J-4 first.
- J-6 + K-2 are tied (hash bump requires CHANGELOG breaking note).
- M-15 `atomic_write` + M-18 self-heal save touch `state.rs` adjacency
  but disjoint regions — parallel OK.
- N-1 ghost docs and N-6 CHANGELOG crates land near each other in
  `docs/`/CHANGELOG; sequential commit recommended for clean history.

---

# Plan source / context

This pass-3 distills:

| Source | Document |
|---|---|
| Deep review of PR #5 | `~/AI_notes/projects/aicx/reports/2026-05-21_pr5-bug-tracker-pass2-deep-review_claude.md` |
| Pass-2 close-out + STOP-POINT-HANDOFF | `~/.vibecrafted/artifacts/Loctree/aicx/2026_0521/bugtracker-aicx-pass-2/STOP-POINT-HANDOFF.md` |
| Pass-2 plan (closed) | `docs/bug-tracker-aicx-followup-pass-2.md` |
| Pass-1 final findings (closed) | `~/AI_notes/projects/aicx/reports/2026-05-20_all-findings-final-session-summary.md` |

**Per-task evidence chain (Pass-3 task ↔ review raport section ↔ source SHA in PR #5):**

| Pass-3 | Review raport finding | Source SHA in PR #5 chain |
|---|---|---|
| J-1 | P1-release-signing | `.github/workflows/release-linux.yml` (PR #5 baseline) |
| J-2 | P1-cross-pin | `Cross.toml` (PR #5 baseline) |
| J-3 | P1-auth-rng-portable | `src/auth.rs` (pass-1 era) |
| J-4 | P1-auth-windows-acl | `src/auth.rs` (pass-1 era) |
| J-5 | P1-auth-toctou | `src/auth.rs` (pass-1 era) |
| J-6 | P1-hash-splitting | `src/state.rs` G-1 era (`0c9ba5e`) |
| J-7 | P1-gcp-redact-limit | `src/redact.rs` (pass-1 era) |
| K-1 | P1-fake-test | `tests/dashboard_security.rs` (pass-1 era) |
| K-2 | P1-changelog-migration | `CHANGELOG.md` Unreleased (G-1 era) |
| L-1 | (pass-2 W-D-4 audit-only) | `26d8123` |
| L-2 | (pass-2 W-D-2 verdict) | operator-side smoke |
| L-3 | (pass-1 A13 + pass-2 replay) | cross-repo |
| M-1..M-23 | P2-* review findings | various, see review raport |
| N-1..N-12 | P3-* review findings | various, see review raport |

**Append to `docs/BUGFIXES.md` per task on close-out.** Update
`docs/BACKLOG.md` items where applicable.

---

_Pass-3 plan written 2026-05-21 by Claude operator-agent after Plan A
(PR #5 follow-up commits `2fb1ccf` + `d2c30aa`) landed. Read with
`vc-init` / `vc-scaffold` at next session start to validate against
then-current HEAD before dispatching._
