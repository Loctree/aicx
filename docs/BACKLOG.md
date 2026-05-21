# BACKLOG — append-only

Lista rzeczy *niezrobionych* surfaced ad-hoc w sesjach. Jeden plik dla całego runtime
(aicx-primary, cross-repo gdzie dotyka aicx). **Append-only.** Najnowsze wpisy na dole.
**Nigdy nie truncate.** Gdy item jest done — odznacz `[x]` z datą i refem do commita/PR,
ale wpisu nie usuwaj.

## Protokół

```
- YYYY-MM-DD [scope] one-line of what's undone. Recover: <command|path|decision>. — open|investigating|done(@ref)
```

`scope` legend: `aicx/<area>` · `loctree/<area>` · `loct-io/<area>` · `plugin/<area>` ·
`ops` · `meta`.

Większe sprawy z forensyką → osobny plik w `docs/incidents/<date>_<slug>.md`,
referencowany z wpisu.

Bez ozdobników. Bez hype. Bez "production-ready". Fakty + ścieżka recovery.

---

## 2026-05-12 — surfaced w trakcie aicx vc-init + clipboard fail z CodeScribe

- 2026-05-12 [aicx/cli] `aicx all -H 100` fails na bucket validation `local/.scripts` — regex `[a-z0-9][a-z0-9._-]{0,99}` w `src/store.rs:459` odrzuca segment zaczynający się od kropki. Cały chunk-phase pada po 41.7s, store nieaktualizowany. Recover: znajdź offending session w canonical store (`grep -rn "local/\.scripts" ~/.aicx/store/` lub state.json), albo (a) zmień bucket name na konwencyjny, (b) rozluźnij validator żeby tolerował `<root>/<dotdir>` jako edge-case. — done(@4ef7410 + walidator relax 2026-05-12 — `.scripts`/`.aicx`/`.github`/`_internal` teraz validator-OK directly; xdot-/xunder-/xdash- escape z 4ef7410 wycofany jako nadgorliwy)
- 2026-05-12 [aicx/store] **Recovery z quarantine 20260509_023025**: ~89k chunks (815 MB) odzyskanych z masowego over-quarantine z 9 maja, plus dragon-side history z `div0:/Users/maciejgad/.aicx/store/` i xcia legacy `~/xcia./store/`. Store: 9k → 121k chunks (98 MB → 1.1 GB). Mechanizm: 3× `rsync -av --ignore-existing` dorzucający tylko brakujące pliki, zachowujący fresh state. — done(2026-05-12, no commit — fs-only operacja)
- 2026-05-12 [aicx/validator] **Walidator relax**: `is_valid_repo_bucket_name` z lowercase-only do case-preserving (CamelCase GitHub orgs `LibraxisAI`, `VetCoders`, `Loctree`, `Szowesgad`) plus dot-prefix (`.aicx`, `.github`, `.scripts`) i underscore-prefix (`_internal`) jako allowed leading chars. `canonical_project_slug` lowercase normalization usunięta — case zachowywany. Doctor scan refactor: usunięty `canonicalize_bucket`/`merge_dir_ignore_existing`/`remove_empty_dirs_recursive` (dead code po relaxach), `CorpusBucketScan` struct → flat `Vec<String>`. Plus `--dry-run` flag dla `--fix-buckets`. Re-run doctor: 113 buckets quarantined do `~/.aicx/quarantine/20260512_075826/` (16 MB realnego junk: `${RELEASE_REPO}`, `<YOUR_USERNAME>`, fragmenty extracted-z-tekstu typu `Loctree/loctree.git\ncd`, markdown content jak `gitbutlerapp/gitbutler\n**Stack**:`). 296/296 lib tests pass. — done(pending commit, src/validation.rs +50/-15, src/store.rs net-cleanup, src/doctor.rs +88/-180, src/main.rs +14/-1)
- 2026-05-12 [aicx/doctor] `aicx doctor --fix` jest no-op dla większości warning classes (sidecars, index_consistency, empty_bodies). Wbrew nazwie nic nie naprawia, ten sam raport. UX gap: `--fix` powinien albo (a) faktycznie wywołać odpowiednie recovery commands (`store --full-rescan`, `--prune-empty-bodies`), albo (b) `--fix` powinno być usunięte/zrebrandowane na `--check` jeśli to tylko lint. — open
- 2026-05-12 [aicx/doctor] `--prune-empty-bodies` emit-script-only design jest defensywą z czasów wczesnego AICX, niepraktyczna dla 12k+ chunks (operator nie zrobi review per-line, więc i tak `bash /tmp/prune.sh` blind). Klasyfikacja jest deterministyczna (`chunk_body_is_empty` + 50-char threshold), nie heurystyczna. Realna naprawa: dodać `--apply` modifier który **MOVE'uje** empty chunks do `~/.aicx/quarantine/empty-bodies-<timestamp>/` (analogicznie do bucket quarantine — recoverable rename, nie outright `rm`). Plus integracja z `--fix` gdy severity=Critical. ~30 LOC + tests. — open
- 2026-05-12 [aicx/corpus] `aicx corpus repair --apply` raportuje `repaired_files: 0, skipped_files: 31248`. Komunikat misleading: jeśli charter ("no inventing/summarizing semantic content") faktycznie zabrania ruszać `internal_thought_frame` (głównej noise class — 31194 z 31248), to repair powinien explicite powiedzieć "skipped because human review required" zamiast pokazywać liczbę kandydatów którzy nigdy nie będą naprawieni. — open
- 2026-05-12 [aicx/store] `aicx doctor`: 534/113593 chunks bez sidecarów (99% coverage). Recover: `aicx store --full-rescan` (sugerowane przez doctor). — open
- 2026-05-12 [aicx/store] `aicx doctor`: index_consistency warning — 1714 orphaned tuples + 282 missing tuples w `~/.aicx/index.json` vs canonical store. Sample orphans: `${RELEASE_REPO}/releases/claude/2026_0419` (template placeholder leaked into bucket — separate bug?), `.../rust-memex/claude/2026_0423`, `BerriAI/litellm/claude/2026_0325`. Recover: `aicx store --full-rescan` żeby zreconcilować. — partial (post-restore: 1408 orphaned + 2224 missing — większość orphans to teraz nazwy quarantined w 20260512_075826, missing to restored chunks bez index entry; nadal recoverable via `aicx store --full-rescan`)
- 2026-05-12 [aicx/empty-bodies] **CRITICAL** post-restore: empty_body_chunks wzrosły z 3696 do 12788 (5.71%) bo restore dorzucił dużo codex sesji z empty internal_thought frames (frame_kind: internal_thought:8235, user_msg:3547). Recovery: `aicx doctor --prune-empty-bodies` emits cleanup script (NIE applies). Decision: review skript + apply, lub zaakceptować jako known-noise. — open
- 2026-05-12 [aicx/index-freshness] semantic_health Warning: 88135s (24h+) lag w semantic index po massive restore. Recovery: `aicx index --dry-run` żeby zobaczyć diff, potem apply variant żeby refresh. — open
- 2026-05-12 [aicx/sanitize] **Self-echo filter hardkod**: `SELF_ECHO_PATTERNS` to `const &[&str]` z 21 patternów, **duplikat w `src/sanitize.rs:474` i `crates/aicx-parser/src/sanitize.rs:481`** (DRY violation, bug-in-waiting przy zmianach jednostronnych). Niepełne coverage: brakuje większości CLI subcommandów (`aicx all -H`, `aicx doctor`, `aicx state`, `aicx corpus`, `aicx index`, `aicx warmup`, `aicx wizard`, `aicx config`, `aicx steer`, `aicx tail`, `aicx ingest`, `aicx migrate`, `aicx sources`, `aicx serve`, `aicx extract`), MCP response side (`"result":{...}`), dashboard HTTP responses. Plus zero konfiguracji w `~/.aicx/config.toml` — wymaga recompile dla custom pattern. Realna naprawa: (a) de-dupe z aicx-parser jako single SoT, (b) auto-derive CLI subcommand patterns z `Commands::*` enum, (c) MCP catch-all `"jsonrpc":"2.0"` zamiast per-method, (d) `[extraction] extra_self_echo_patterns = [...]` w config. Skala dziś: 396 entries filtered z 872k to **niski recall** filtra — większość operator's CLI invocations dziś (~10+ `aicx X` commands) wjechała do storu jako treść. — partial(@616e6bc, 2026-05-20 — (a) de-dupe done: `src/sanitize.rs` deleted, single SoT teraz w `crates/aicx-parser/src/sanitize.rs`, `aicx_read` MCP tool name dodany do SELF_ECHO_PATTERNS; (b) auto-derive CLI patterns z `Commands::*`, (c) MCP catch-all `"jsonrpc":"2.0"`, (d) config-driven `[extraction].extra_self_echo_patterns` — wszystkie trzy nadal open. Patrz `docs/BUGFIXES.md` 2026-05-20)
- 2026-05-12 [aicx/store] `${RELEASE_REPO}/releases/claude/2026_0419` jako bucket name w canonical store — placeholder template prawdopodobnie wyciekł z workflow. Sprawdzić skąd, dodać validation żeby `${...}` lub niezresolved zmienne nie tworzyły bucketów. — open
- 2026-05-12 [aicx/store] `aicx doctor`: 3696 empty-body chunks (3.25%). Breakdown: agent_reply 474, internal_thought 1282, unknown 61, user_msg 1879. Próbka z codex sessions 2025_0810/0812/0916. Recover: `aicx doctor --prune-empty-bodies` emituje reviewable cleanup script (NIE applies sam). — open
- 2026-05-12 [aicx/extract] CodeScribe extractor source: `[codescribe] 0 entries` w `aicx all` mimo że codescribe runtime istnieje (binarka 0.10.0 zainstalowana). Albo (a) extractor nie wie skąd brać codescribe sessions, (b) codescribe nie zapisuje sessions w trackable formacie, (c) ścieżka jest nieskonfigurowana w aicx config. Sprawdzić: `aicx config list`, `aicx sources audit`. — open
- 2026-05-12 [aicx/branch] `feat/aicx-extract-improvements`: 48 ahead / 65 behind `origin/develop`, 47 ahead / 0 behind `origin/main`. Decision needed: rebase vs merge vs keep diverged. — open
- 2026-05-12 [aicx/wip] 15 unstaged plików w worktree (Cargo.lock/toml v0.7.x→0.7.x, npm distribution sync, doctor.rs/mcp.rs/search_engine.rs/vector_index.rs error message polish — "embedder init failed" → "semantic embedder unavailable (optional)", `enable_tool_list_changed()` w MCP server, tools/release_sync.py +24 lines). 1 stash @ 1e63b01 (npm distribution sync WIP). Decision: które iść do staging dla v0.7.1, które stash, które zostawić. — open
- 2026-05-12 [aicx/test] AICX active intent z 2026-05-08 (z context atlas, label `AicxFailure`): "Gates 1-6 passed (manifest/fmt/check×2/clippy×2). 3 testy z `runtime_cli_store_contract` failed — kanoniczne lowercase assertions. Fix." Verify: czy te 3 testy ciągle failują na current HEAD (`3237157`), czy zostały naprawione i intent jest stale. — investigating
- 2026-05-12 [meta] Brak tracku undone work była głównym source of frustration. Każdy wypływający item ad-hoc trafia tutaj. Nie do `docs/internal/` (gitignored). Ten plik (`docs/BACKLOG.md`) ma być wersjonowany w git. — open

## 2026-05-11 — CodeScribe 0.10.0 segfault + clipboard regression

- 2026-05-11 19:29:49 PDT [codescribe] CodeScribe.app 0.10.0 segfault: `EXC_BAD_ACCESS (SIGSEGV) KERN_INVALID_ADDRESS at 0x0000145a342d9fe8`, faulting thread 0 (main / `com.apple.main-thread`). Top of stack: `objc_release` → `AutoreleasePoolPage::releaseUntil` → `objc_autoreleasePoolPop` → `_CFAutoreleasePoolPop` → `-[NSAutoreleasePool drain]` → `-[NSApplication run]`. Klasyczny use-after-release w obsłudze NSApp event loop autorelease pool — wskazuje na over-release Rust→ObjC bridge object albo dangling reference do view/scrubber (x28 register: `OBJC_IVAR_$_NSScrubberChangeTransition._view`). macOS 26.5 (25F71), Mac15,9, ARM64 native. Codesign ID `com.codescribe.app`, team `MW223P3NPX`. Forensikę zapisano do `docs/incidents/2026-05-11_codescribe-segfault-0.10.0.md`. — open
- 2026-05-11 [codescribe/overlay] CodeScribe overlay przy paste z buffera wkleja header `------------------------------------- / Translated Report (Full Report Below) / -------------------------------------` przed content użytkownika. Garbage prefix w paste flow — zaśmieca każdy paste-through. Fix: clipboard handler nie powinien dorzucać Apple crash report frame'a do user-paste. — open
- 2026-05-11 [codescribe/overlay] CodeScribe overlay autocorrect/transcription zamienia `plik` (Polish: file) na `blik`. Replikowane niezależnie przez user. Fix: dodać Polish dictionary entries albo wyłączyć aggressive autocorrect dla unrecognized stems. — open

## (cross-repo) Loctree-side undone — referencowane z 0.10-prep PR description

> Te wpisy duplikują `feat/lsp/codelens-live-analyzer` PR description w `loctree-suite`,
> ale lądują tutaj żeby cross-repo undone work miał jeden punkt prawdy. Status update
> w obu miejscach.

- 2026-05-11 [loctree/release] `release-bundles.yml` workflow nigdy nie odpalony na realnym tagu — pierwszy run będzie też testem (oczekiwany failure mode: missing GPG key, wrong artifact paths, AICX 0.7.0 niedostępne na release-time). — open
- 2026-05-11 [loctree/release] 0.9.5 publiczny tarball ma regresję LSP — 5/6 binarek (brak `loctree-lsp`). Fix tylko po wystrzeleniu nowego workflow na 0.10.0. Confirmation via `tar -tzf` na wszystkich 3 platformach. — open
- 2026-05-11 [loctree/lsp] Plan 04 client command dla atlas-card opening — server gotowy, klient `editors/vscode/src/commands.ts` nie ma `loctree.openAtlasCard` rejestracji. Plan 15 quickfix dziedziczy ten gap. — open
- 2026-05-11 [loctree/lsp] Plan 08 full symbol scope semantics — staged. — staged
- 2026-05-11 [loctree/lsp] Plan 14 live external AICX runtime end-to-end — mock/integration only. — staged
- 2026-05-11 [loctree/lsp] Plan 16 Stage 2 refactor language-specific edits + benchmarks. — staged
- 2026-05-11 [loctree/lsp] Plan 19 Stage 2 deeper symbol granularity language coverage. — staged
- 2026-05-11 [loctree/npm] npm publish dla wszystkich 15 paczek (3 wrappery + 12 platform) — scaffold gotowy, registry push nie wykonany. — open
- 2026-05-11 [loctree/gates] `make precheck` / `make test` / `cargo clippy --workspace --all-targets -- -D warnings` całościowy gate dla 0.10-prep branch — NIE RUN świeżo (last audit ran tylko `cargo test -p loctree-lsp` i `cargo test -p loctree context_scope --lib`). — open
- 2026-05-11 [loctree/release] Packaging install smoke (curl install na czystym VM, npm install na 3 OSach). — open
- 2026-05-11 [loctree/docs] Plan 23 — wire HTML layer into real analyzer flow (untracked plan w docs/plans/lsp/). — open
- 2026-05-11 [loctree/docs] Plan 24 — polish HTML generated artifacts UI/UX to loctree-com styling (untracked plan w docs/plans/lsp/). — open
- 2026-05-11 [loctree/docs] CHANGELOG.md sync z faktycznym scope 0.10-prep — touched ale nie verified. — open
- 2026-05-11 [loct-io] install bundle URL bump 0.9.5 → 0.10.0 po release (`loct-io/src/sections/install.rs`). — open
- 2026-05-11 [loctree/tracker] TRACKER.md ma `stage_pass` vs `done` distinction dla tasków 08/14/15/16/19. — open
- 2026-05-11 [loctree/lsp] Plan 03 closure-adjusted (CodeLens używa no-op `Command { command: "" }` zamiast literal `command: None` z original task) — udokumentować w task/tracker zamiast zostawiać jako quiet diff. — open
- 2026-05-11 [loctree/audit] Plan 21 source restore (P1 z 22_task_audit_report) — original task file zaginął, jest reconstruction; jeśli oryginał miał inne kryteria, reconstruction może nie matchować. — open

---

*𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by VetCoders (c)2024-2026 The LibraxisAI Team*
