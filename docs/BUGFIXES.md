# BUGFIXES — append-only

Pamiętnik bug-fixera. Każdy wpis to jeden bug ze ścieżką od symptomu do root cause
do fixa, plus lessons learned dla kolejnych agentów / Macieja / przyszłych sesji.

**Append-only.** Najnowsze wpisy na dole. Nigdy nie truncate. Gdy bug wraca w innej
formie — nowy wpis z `Related:` linkiem do poprzedniego, nie edytuj starego.

Powiązania z innymi dokumentami:

- `docs/BACKLOG.md` — lista *niezrobionych* rzeczy (TODO). Po zamknięciu pozycji
  z BACKLOG dodaj wpis tutaj jeśli był to bug (nie chore/refactor/feature).
- `docs/incidents/<date>_<slug>.md` — pełna forensika dla większych incydentów
  (segfault, data loss, security). Wpis BUGFIXES wtedy linkuje do incidentu.
- `CHANGELOG.md` — user-facing release notes (Keep a Changelog). BUGFIXES jest
  agent/dev-facing — głębszy root cause + lessons, mniej polerowane.

## Protokół wpisu

```markdown
## YYYY-MM-DD — short-title · `commit-sha[,sha2...]`

**Symptom.** Co operator/użytkownik widział (1–2 zdania, konkretnie).

**Root cause.** Dlaczego to się działo (techniczne, ze ścieżką wykonania).

**Fix.**
- Co konkretnie zmienione (bullet list).
- Każdy bullet niezależny — łatwiej cherry-pickować podczas analizy.

**Touched.**
- `path/to/file.rs` — funkcja/area
- ...

**Tests.** Dodane / zmienione / usunięte (krótko).

**Lessons.** Pattern albo trap który warto zapamiętać (1–3 bullets, opcjonalne).

**Related.** (opcjonalne) — linki do innych wpisów BUGFIXES, BACKLOG items,
incidents, lub external issues.

---
```

Reguły stylu:

- PL w prozie, EN w identyfikatorach/komendach/ścieżkach.
- Bez „production-ready", bez emoji, bez hype.
- Fakty + ścieżka wywołania + co konkretnie tknięte. Nie referat.
- Lessons pisz jak do kolegi po fachu, nie do menedżera.

---

## 2026-05-19 — segmentation identity leak · `d5d7da5`, `49520a8`

**Symptom.** ~10 bogus projects w `~/.aicx/store` (Homebrew/brew, openai/codex,
RustSec/advisory-db, thedotmack/claude-mem, Szowesgad/maciej-almanach, …),
populated realnymi chunkami z innych sesji. Pojedyncza sesja vista-codex
trafiała do bucketu Szowesgad/maciej-almanach. Równolegle `aicx search -p vista`
balonował do 7 projektów / ~32k chunków zamiast jednego.

**Root cause.** Dwie ortogonalne luki w pipeline:

1. `infer_tiered_identity_from_text` walking dowolnej absolute path w treści
   chunka przez `discover_git_root` + `git remote get-url origin` → zwracał
   remote-URL identity na tier Primary/Secondary (assertable). Chunk który
   tylko *wspominał* lokalną ścieżkę `/Users/foo/Downloads/ai-collaborators`
   (lokalny klon `Szowesgad/maciej-almanach`) hijackował segment.repo.
2. `store.rs` `repo_slug.to_lowercase().contains(filter)` substring match na
   trzech path-segmentach. `-p vista` matchował `vista-portal`, `VistaBrain`,
   `vista-datasets`, `nextra-docs-vista`.

Round-2 (4h później) wykrył dalej: nawet po stopnięciu FS-walka,
`infer_tiered_identity_from_entry` wołał `infer_tiered_identity_from_text`
jako trzeci fallback. URL mentions wracały do segment.repo jako Fallback tier
i powodowały false `context_switch` (split sesji na środku) oraz smear
segment.repo non-ownership signałami.

**Fix.**

- `segmentation.rs`: drop path-regex branch w `infer_tiered_identity_from_text`.
  Zostaje tylko `https://github.com/X/Y` URL mention, clamped do non-assertable
  Fallback tier.
- `segmentation.rs`: case-insensitive markers w `infer_repo_identity_from_known_layout`
  — macOS `/Users/u/Git/...` resolwuje przez cwd zamiast wpadać w text inference.
- `segmentation.rs` (round-2): `infer_tiered_identity_from_entry` przestaje
  wołać text inference jako trzeci fallback. Entry-level identity wyłącznie
  z cwd / projectHash registry. Text mentions zostają dostępne przez standalone
  `resolve_bucket` API (`BucketingSource::ContentMention`) jako search-hint
  surface — nigdy nie zasila segment routing.
- `store.rs`: substring match → strict `project_filter_matches` z 4 formami:
  `owner/repo` (slug), `owner/` (org wildcard), `/repo` (cross-org repo
  wildcard), `name` (cross-org exact name).
- `store.rs`: `resolve_filters_to_slugs` enumeruje canonical store i mapuje
  short user input na `<owner>/<repo>` slug.
- `main.rs`: każde `-p` route'owane przez resolver przed downstream lookup.
  Typo fail-fast (już nie silent `_all`).
- `main.rs`: clap `-p` z `num_args = 1..` (greedy multi-value zjadało
  positional QUERY) na default single value per occurrence.
- `vector_index.rs`: stale tmp-checkpoint mismatch raportuje diverging
  schema/model/profile/dim fields + sugeruje `rm <path>`.
- `sources.rs`: Gemini path-ownership — session path
  `~/.gemini/tmp/<project>/chats/...` wins nad message-content cwd hints.

**Touched.**

- `crates/aicx-parser/src/segmentation.rs` — identity inference rewrite.
- `src/store.rs` — `project_filter_matches`, `resolve_filters_to_slugs`.
- `src/main.rs` — clap defs + resolver wiring + warning emit.
- `src/sources.rs` — Gemini path ownership.
- `src/vector_index.rs` — checkpoint divergence reporting.

**Tests.** `aicx-parser` 108 → 115 pass. `aicx` (lib) 347 → 363 pass.
Workspace 510/510 lokalnie (uwaga: 2 testy clap-side były pre-existing failing,
patrz wpis 2026-05-20 — `-p` test alignment). Nowe regression:
`entry_identity_ignores_text_url_mentions`,
`resolve_bucket_still_surfaces_text_url_mentions`,
`semantic_segment_does_not_split_on_text_url_mention`.

**Lessons.**

- **Identity inference z treści chunka jest fundamentalnie niebezpieczne
  dla store routing.** Co chunk *mówi* o sobie ≠ do kogo *należy*. Ownership
  ma wynikać z deterministycznych źródeł (cwd, registry, frame-level
  provenance). Text mentions to search-hint signal, nie ownership signal.
- **Substring match na user-input filterach które trafiają do path-resolving
  zawsze wybuchnie.** Strict exact match + explicit wildcards (`owner/`,
  `/repo`) > magic substring inference.
- **Round-1 vs Round-2.** Pierwszy fix zatrzymał objaw (FS-walk), ale
  systemowa ścieżka (fallback chain) była jeszcze otwarta. Partner review
  / drugie spojrzenie po godzinach łapie to czego natychmiastowy fix nie
  widzi. Buduj rytm: fix → przerwa → review → round-2.

**Related.** Wymagało następnie `-p` test alignment (wpis 2026-05-20)
oraz pełnego sweepa `-p` semantics (wpis 2026-05-20).

---

## 2026-05-20 — atomic store writes + UUIDv7 basename collision precheck · `bc67728`

**Symptom.** Trzy niezależne ścieżki w canonical store mogły po cichu
tracić dane:
- `truncate_session_id` brał pierwsze 12 znaków session ID; dwa UUIDv7
  z bliskich timestampów dzielą ten prefix → drugi chunk po cichu
  nadpisywał pierwszy (`fs::write` bez `path.exists()` precheck).
- `fs::write` na chunk `.md`, sidecar `.meta.json`, `index.json`
  i migration manifestach nie był atomowy; crash mid-write zostawiał
  truncated plik lub orphan `.md` bez sidecara.
- `load_index_at` robiło `serde_json::from_str(...).unwrap_or_default()`
  — uszkodzony `index.json` cicho resetował cały index do pustki,
  bez warna, bez fallbacka.

**Fix.**
- Nowy moduł `src/store/atomic_write.rs`: `atomic_write` (tempfile +
  fsync + rename + best-effort parent fsync) oraz dwufazowy
  `stage_tempfile` / `commit_tempfile` / `discard_tempfile` dla
  uporządkowanych renamów wielu plików.
- `truncate_session_id`: limit 12 → 20 znaków cleaned + sufix
  `-h{6-char-siphash13-hex(session_id)}` gdy nastąpiło truncation;
  prefix-collision UUIDv7 niemożliwy.
- `write_context_session_first_outcome_at`: `target.exists()` precheck;
  jeśli istniejący sidecar ma **inny** `content_sha256`, zapisujemy
  pod `<stem>-c{6-char-siphash13-hex(content_sha256)}.md` i emitujemy
  `tracing::warn!`. Identyczny `content_sha256` → dedup (poprzednia
  ścieżka się nie zmieniła).
- Sidecar commit order: stage chunk-tempfile → stage sidecar-tempfile
  → commit `.md` → commit `.meta.json`. Crash między renamami daje
  detectable orphan `.md` (bez sidecara), nigdy orphan + zły sidecar.
- `load_index_at` zwraca teraz `Result<StoreIndex>`: `tracing::warn!`
  + próba `.bak` sibling → `Err` jeśli oba się nie udadzą. `save_index_at`
  używa `atomic_write` i robi best-effort `fs::copy(...index.json.bak)`
  PRZED swapem. Publiczne `load_index()` zachowuje API (`StoreIndex`)
  mapując `Err` na `default()` z warnem.
- Pozostałe `fs::write` w produkcyjnych ścieżkach (`write_context`,
  `write_context_chunked`, `write_migration_artifacts`, provenance
  JSON) → `atomic_write`.

**Touched.**
- `src/store/atomic_write.rs` — nowy moduł, 4 testy.
- `src/store.rs` — `mod atomic_write`, `truncate_session_id` rewrite,
  `siphash13_hex6`, `load_index{,_at}`, `save_index_at`,
  `write_context_session_first_outcome_at` (collision precheck +
  two-phase commit), sweep wszystkich `fs::write` w produkcyjnych
  ścieżkach, usunięty redundantny `write_chunk_sidecar`. 5 nowych
  testów regresyjnych.

**Tests.** 9 nowych: 4 w `atomic_write::tests` (create / overwrite /
tempfile-cleanup-on-error / unicode paths), 5 w `store::tests`
(UUIDv7-prefix collision end-to-end, existing target z innym contentem
→ disambiguation, identyczny content → dedup, crash simulation, malformed
`index.json` → `Err` + `.bak` recovery). Wszystkie wcześniejsze testy
w `store.rs` przechodzą; `cargo test --lib -p aicx` = 398 passed.

**Lessons.**
- Prefix UUIDv7 to time-domain leak: 12 hex chars to ~6 godzin entropii,
  nie 2^48 jak komentarz sugerował. Truncation musi nieść hash sufiks
  pełnego ID.
- `unwrap_or_default()` na parse JSON-a indexa = silent thief klasy
  „1714 orphan / 282 missing" (BACKLOG 2026-05-12). Każdy korupcjogenny
  fallback powinien iść przez `tracing::warn!` + `.bak` recovery,
  nigdy cicho do default.
- Dwufazowy commit (`stage` + `commit`) jest jedynym sposobem na
  uporządkowane renamy dwóch plików; sekwencyjne `atomic_write × 2`
  daje orphan przy crashu między nimi.

**Related.** Area B Wave-A (B-1 P0 + B-4 P1 + B-5 P1) z
`/Users/silver/Downloads/bug-tracker-aicx.md` linie 645-1058.
SUBAGENT_02 audit:
`/Users/silver/AI_notes/projects/aicx/reports/subagents/SUBAGENT_02_audit-area-B--20-05-2026.md`.
Wave-2 (`state.rs` atomic save + outer state lock w `run_store`)
zużyje ten sam `atomic_write` helper.

---

## 2026-05-19 — date-shaped names accepted as repo · `49520a8`

**Symptom.** Pseudo-projekty typu `CodeScribe/2026-01-22`, `CodeScribe/2026_01_22`,
`CodeScribe/2026_0122` w canonical store. Każda sesja w której fragment treści
wyglądał jak data produkowała nowy fałszywy bucket.

**Root cause.** `is_probably_repo_name` filtr akceptował alfanumeric + `.-_`,
więc `2026-01-22`, `2026_01_22`, `2026_0122` wszystkie przechodziły jako
„prawdopodobnie nazwa repo".

**Fix.** Nowy `looks_like_date_pattern` guard rejecting 3 shapes (`YYYY-MM-DD`,
`YYYY_MM_DD`, `YYYY_MMDD`). Shape-only — nie waliduje calendar months/days,
bo intent matters: cokolwiek wyglądającego jak data nie ma być repo-name.

**Touched.**

- `crates/aicx-parser/src/segmentation.rs` — `looks_like_date_pattern` + plug
  do `is_probably_repo_name`.

**Tests.** `is_probably_repo_name_rejects_date_patterns`,
`looks_like_date_pattern_recognizes_three_shapes`.

**Lessons.**

- **Format validation na user-derived identifierach powinien rejectować
  „looks like X" shapes** (daty, hashe, timestamps, UUID-y) zanim te shapes
  rozprzestrzenią się po store jako pseudo-projects.
- **Shape rejection > calendar validation.** Walidacja czy `2026-13-45` to
  poprawna data wymagałaby roku/miesiąca/dnia parsing — przesada. Reject
  wszystko co *wygląda* jak data; jeśli ktoś nazwał repo `2026-01-22`,
  niech zrobi `git remote set-url`.

**Related.** Część segmentation identity leak round-2 (`49520a8`).

---

## 2026-05-19 — silent `-p` bare-name ambiguity · `49520a8`

**Symptom.** `aicx search -p codex` matchował zarówno organizację `codex/*`
(wszystkie repo pod `codex`) jak i repo `*/codex` (wszystkie organizacje
mające repo o nazwie `codex`). Operator dostawał union bez wiedzy że to
ambiguous, więc widział „za dużo wyników" bez powodu.

**Root cause.** `resolve_filters_to_slugs` po prostu unionował matche bare-name
bez detekcji że to jednocześnie org-name i repo-name. Operator nie miał
żadnego sygnału.

**Fix.** `detect_ambiguous_bare_filter` w resolverze wykrywa kolizję
(name żyje jako org *i* repo). `resolve_project_filters_or_error` emituje
stderr warning naming oba matche i sugerujący `-p name/` (org-only) lub
`-p /name` (repo-only). Behavior unchanged — nadal union — ale warning
removes the WTF.

**Touched.**

- `src/store.rs` — `detect_ambiguous_bare_filter`.
- `src/main.rs` — warning emit w resolver wrapper.

**Tests.** `detect_ambiguous_bare_filter_*` (dwa testy: pure-org match,
ambiguous match).

**Lessons.**

- **Silent unions na ambiguous user input** są źródłem „dlaczego to matchnęło
  tyle rzeczy" w długim ogonie. Loud warning + unchanged behavior > silent
  surprise + breaking change.
- **Sugeruj explicit syntax w warning message** zamiast tylko opisywać
  problem. Operator który widzi „użyj `-p name/` lub `-p /name`" wyklikuje
  fix w sekundę.

---

## 2026-05-20 — dead sanitize twin + cutoff overflow · `616e6bc`

**Symptom.** Dwa odłożone bugi w jednym refactor:

1. Zmiana `SELF_ECHO_PATTERNS` w `src/sanitize.rs` nie miała wpływu na
   runtime. Operator dorzucił pattern, recompile, nic się nie zmieniło.
2. `aicx ... -H <huge-u64>` mógł postawić cutoff w przyszłości i zwrócić
   pusty wynik bez błędu (jeśli intencja była lookback all-time).

**Root cause.**

1. `src/sanitize.rs` (622 linii) **nigdy nie był kompilowany**. `src/lib.rs:40`
   już re-eksportował `aicx_parser::sanitize` jako `aicx::sanitize` (bin
   path) i `crate::sanitize` (lib path). Lokalny plik nie był podpięty do
   mod tree i Cargo nie ostrzega o tym w bin crate. Edycje szły w void.
2. `cutoff_for_hours` używał `u64 as i64` cast w arytmetyce timestamp.
   `u64 as i64` w Rust to bit-cast (nie saturating). Wild u64 = ujemne i64
   = cutoff przesunięty w przeszłość/przyszłość poza intencją.

**Fix.**

- Delete `src/sanitize.rs`. Carry-forward jedynego elementu który lokalny
  plik miał a parser nie: retired `aicx_read` MCP tool name → dodany do
  `aicx_parser::sanitize::SELF_ECHO_PATTERNS` żeby historyczne traces
  pozostały filtrowalne.
- Collapse `cutoff_for_hours` + `lookback_cutoff` w jeden canonical
  `lookback_cutoff`:
  - `hours == 0` → `all_time_cutoff()` (operator convention: 0 = all).
  - `hours > 0` → `Utc::now() - hours`, clamped do `[1, i32::MAX]`. Wild
    u64 nie może już silently wrapnąć `as i64` w przyszłość.
- Update 2 callerów (`run_conversations_batch`, `run_search` filter) +
  rewrite 3 cutoff testów pod nową nazwę + dodaj `zero_returns_all_time`.

**Touched.**

- `src/sanitize.rs` — **deleted** (622 linii).
- `crates/aicx-parser/src/sanitize.rs` — carry-forward `aicx_read` pattern.
- `src/main.rs` — `lookback_cutoff` unification + clamping.

Net diff: +31 / −660.

**Tests.** 3 cutoff testy rewritten + `lookback_cutoff_zero_returns_all_time`.

**Lessons.**

- **W bin crate plik istniejący ≠ plik kompilowany.** Cargo nie warnuje
  o niereferencjonowanych `*.rs` w `src/` bin crate jeśli nie są w mod
  tree. Gdy widzisz DRY violation pomiędzy dwoma lokalizacjami, sprawdź
  `git grep "mod sanitize"` / `cargo expand` / Loctree które re-eksporty
  są live a które dead. Edytowałeś plik a nic się nie zmienia → najpierw
  sprawdź czy plik jest w binary.
- **`as i64` na user-provided integer to bit cast, nie saturating.**
  Dla arytmetyki która ma sens dla człowieka (godziny, dni, sekundy)
  clamp do safe range zanim castujesz, albo użyj `checked_*` /
  `saturating_*`. Default Rust integer semantics ≠ default human
  semantics.

**Related.** Resolves DRY violation flagged w `docs/BACKLOG.md` (2026-05-12
self-echo entry).

---

## 2026-05-20 — `-p` clap contract sweep · `ed6fb0e`, `1ea4313`, `0bfa73b`

**Symptom.** Dwa wymiary:

1. Workspace test failure: `index_accepts_multiple_project_filters` i
   `search_accepts_multiple_project_filters` padały już na
   `fix/segmentation-identity-leak` (commit message twierdził „510/510 pass",
   ale dla tych dwóch testów było inaczej). Cherry-pick na `fix/broken-indexing`
   przeniósł problem rzetelnie.
2. Brak spójności semantyki `-p` w CLI: Search i Index miały
   single-value-per-occurrence (`value_delimiter = ','`), a Intents, Steer,
   Tail oraz wszystkie extract/store komendy (`claude`, `codex`, `all`,
   `conversations`, `store`, `ingest`) nadal miały greedy `num_args = 1..`.
   Operator pisał `aicx search -p a b` (fail) i `aicx all -p a b` (works) —
   ostry inconsistency.

**Root cause.**

1. `d5d7da5` zmienił clap `-p` z `num_args = 1..` (greedy) na single-value
   per occurrence dla Search/Index — żeby `vibecrafted` jako positional
   QUERY nie wpadało jako trzecia wartość `-p`. Ale companion testy
   (`-p vc-operator vibecrafted -p loctree`, oczekujący 3 elementy)
   zachowały starą greedy formę. Test był relikt poprzedniej semantyki.
2. Sam `d5d7da5` tknął tylko Search/Index. Reszta `-p` została
   z `num_args = 1..` „bo w nich greedy nie zjadał pozytional", więc
   contract się rozjechał.

**Fix.** Trzy follow-up commity:

- `ed6fb0e`: testy Search/Index do `-p a -p b -p c` repeated formy + drop
  „space-list" z error message. 4 linijki, 4/4 testów multiple_project_filters
  zielone.
- `1ea4313`: drop `num_args = 1..` z Intents i Steer dla spójności
  z Search/Index. Update companion testów. Plus `steer --help` test:
  assertion `--project <PROJECT>...` → `--project <PROJECT>` (clap renderuje
  `...` tylko dla multi-value glyph, którego już nie ma).
- `0bfa73b`: full sweep — pozostałe 7 komend (`claude`, `codex`, `all`,
  `conversations`, `store`, `ingest`, `tail`) też dropped `num_args = 1..`
  i adopt `value_delimiter = ','`. Żaden test nie używał greedy formy dla
  tych komend, więc to czysto contract-tightening.

Po sweepie każdy `-p` w CLI obowiązuje jeden kontrakt:

```
-p owner/repo    strict slug
-p owner/        org wildcard
-p /repo         cross-org repo wildcard
-p name          cross-org name match
-p a,b -p c      repeated and/or comma list form a union
-p a b           no longer greedy (was bug-prone next to QUERY)
```

**Touched.**

- `src/main.rs` — 10 clap defs (`#[arg(short, long, ...)]`), 4 testy,
  1 help-render assertion.

**Tests.** Wszystkie test workspace zielone — `cargo test --workspace
--no-fail-fast` EXIT=0.

**Lessons.**

- **Pół-fix inconsistency jest gorszy od niefixa.** Gdy zmieniasz contract
  dla N subcommendów, dopnij wszystkie naraz lub explicite pozostaw TODO
  z deadline. Operator UX rozjeżdża się subtelnie — `aicx search -p a b`
  failuje, `aicx all -p a b` works — i wraca jako „dlaczego ten CLI jest
  taki niespójny".
- **Help-content asserty są sprzężone z arg semantyką.** Test
  `rendered.contains("--project <PROJECT>...")` zakładał `num_args = 1..`
  bo clap renderuje glyph `...` tylko dla multi-value. Drop num_args → drop
  glyph → drop assertion suffix. Pamiętaj że zmiana contract clap
  bezpośrednio zmienia output `--help`.
- **„Workspace pass" w commit message wymaga weryfikacji ścieżką**
  `cargo test --workspace --no-fail-fast`, nie selektywnym `cargo test
  --lib`. `d5d7da5` deklarował 510/510 — dwa testy faktycznie failowały
  i prawdopodobnie operator uruchamiał test innym targetem.

**Related.** Bezpośredni follow-up do segmentation identity leak
(2026-05-19) — tamten fix zmienił clap, ten domknął kontrakt na całej
powierzchni.

---

## 2026-05-20 — modern secret redaction coverage (Area C P1) · `0b1b7ad`

**Symptom.** `redact_secrets` przepuszczał współczesne credential formats:
Anthropic `sk-ant-api03-*`, OpenAI `sk-proj-*`, GitHub `ghs_/gho_/ghu_/ghr_`,
GitLab `glpat-*`, AWS `ASIA*`, Slack `xapp-*`, JWT, Stripe `sk_/rk_` oraz
GCP service-account JSON z escaped `private_key`.

**Root cause.** Warstwa regexów była prefix-anchored na starsze formaty
(`sk-*`, `ghp_*`, `xox*`, `AKIA*`, `AIza*`) i raw PEM block. JSON field
`"private_key": "-----BEGIN...\n..."` oraz service-account side fields
nie miały osobnej ścieżki redakcji.

**Fix.**
- Dodano explicit regexy dla nowych rodzin tokenów i wpisy w `SECRET_LOOKUP_SET`.
- `ghp_*` przeniesiono do wspólnego `RE_GITHUB_TOKENS_EXT`, żeby nie robić
  podwójnej redakcji.
- Dodano GCP service-account JSON redaction: `private_key` globalnie,
  a `private_key_id` i `client_email` tylko w bounded obiekcie
  `"type": "service_account"`.
- Zachowano istniejące labelki i patterny legacy.

**Touched.**
- `src/redact.rs` — regex set, replacement pipeline, GCP SA helper, unit tests.
- `tests/secret_redaction_e2e.rs` — output-level md/json no-leak regression.

**Tests.** Dodano pozytywne testy dla nowych rodzin, GCP SA JSON i Bearer
regression, negatywne testy SHA1/SHA256/UUID/base64/patient-like oraz e2e
test piszący conversation `.md` + `.json` przez `aicx::output`.

**Lessons.**
- Token regex coverage musi być pinowane pozytywnymi i negatywnymi testami
  razem; samo poszerzenie patternów bez SHA/UUID/base64 guardów to proszenie
  się o false positives.
- JSON-escaped private keys nie są tym samym przypadkiem co raw PEM block;
  potrzebują field-aware redakcji przed ogólnym PEM replacerem.

**Related.** Area C P1 1.1-1.10 z `/Users/silver/Downloads/bug-tracker-aicx.md`.

---

## 2026-05-20 — frontmatter parser skip-unsupported-value (A-10) · `202d5d1`

**Symptom.** Frontmatter parser wyrzucał cały blok metadanych, jeśli pojedyncza
wartość wyglądała jak unsupported YAML shape (`[...]` albo `{...}`). W ingest
to oznaczało utratę sąsiednich scalarów typu `run_id`, `prompt_id`, `status`,
`model` i `started_at`.

**Root cause.** `parse_frontmatter_fields` robił `return None` wewnątrz pętli
po liniach, gdy `looks_like_unsupported_yaml_value(value)` zwracało true.
Jedną nieskalarną wartość traktował więc jak błąd całego frontmattera zamiast
pominąć tylko ten klucz.

**Fix.**
- `parse_frontmatter_fields`: unsupported value branch robi teraz `continue`,
  więc parser pomija tylko bieżący key/value i zachowuje pozostałe scalars.
- Test `handles_malformed_yaml_gracefully` przepisany z oczekiwania
  `frontmatter.is_none()` na zachowanie sąsiednich scalarów.
- Dodano regresje dla unsupported list, map-shaped value i tekstu bez
  delimiterów frontmatter.
- Downstream chunker test zaktualizowany do nowego kontraktu: unsupported
  `run_id` nie blokuje zachowania scalar `mode`.

**Touched.**
- `crates/aicx-parser/src/frontmatter.rs` — `parse_frontmatter_fields`, tests.
- `crates/aicx-parser/src/chunker.rs` — test alignment dla frontmatter ingest.

**Tests.** `cargo test --package aicx-parser --lib` — 118/118 pass. Root
`make precheck`, `make test`, `make check`, `make clippy` były odpalone, ale
blokuje je równoległy out-of-scope diff w `crates/aicx-embeddings/src/cloud.rs`
(`CloudEmbeddingProvider` zdefiniowany/importowany dwa razy). `make fmt` pass.

**Lessons.**
- Parser frontmatter powinien fail-soft na pojedynczych unsupported value
  shapes; telemetry scalars są cenniejsze niż perfekcyjna obsługa każdego
  sąsiedniego YAML idiomu.
- Testy downstream mogą utrwalać stary bug nawet wtedy, gdy unit test parsera
  został już poprawiony — po zmianie kontraktu sprawdź consumer tests w tej
  samej paczce.

**Related.** A-10 z `/Users/silver/Downloads/bug-tracker-aicx.md`, Area A.

---

## 2026-05-20 — transactional state.json + outer state lock (B-2+B-3) · `31186db`

**Symptom.** Dwa równoległe `aicx store` / `aicx all` mogły załadować ten sam
stary `state.json`, a potem drugi zapis nadpisywał watermarki, `seen_hashes`
i `runs` pierwszego. Dodatkowo crash podczas `StateManager::save` mógł zostawić
ucięty `state.json`, a parse failure cicho resetował state do defaultu.

**Root cause.** `StateManager::load` i `save` brały `state.lock` tylko na
krótkie fragmenty pojedynczego odczytu/zapisu. Cały cykl read → mutate → save
w `run_store` i `run_extraction` działał bez outer locka. Sam save używał
`fs::write`, a load robił `serde_json::from_str(...).unwrap_or_default()`.

**Fix.**
- `StateManager::load` zwraca teraz `Result<Self>`; brak pliku nadal daje
  default, ale malformed JSON próbuje `state.json.bak`, a bez backupu zwraca
  jawny błąd zamiast silent default.
- `StateManager::save` używa Wave-1 `atomic_write` i zapisuje rolling
  `state.json.bak` z poprzedniej wersji przed atomową podmianą main file.
- Wewnętrzne `state.lock` reacquire w `load/save` zostało usunięte, żeby
  outer lock nie deadlockował na tym samym zasobie.
- `run_extraction`, `run_store` i `run_state` trzymają teraz exclusive
  `state.lock` przez pełny state read-modify-write cykl.

**Touched.**
- `src/state.rs` — transactional load/save, backup recovery, state unit tests.
- `src/main.rs` — outer `state.lock` w `run_extraction`, `run_store`,
  `run_state`.
- `src/store.rs` — `atomic_write` module visibility only; store write paths
  nietknięte.
- `tests/locks_contention.rs` — simplified concurrent state update regression.

**Tests.** Dodano 4 state unit tests i 1 integration contention test. Zielone:
`cargo test --package aicx --lib state::` (26/26), `cargo test --test
locks_contention` (2/2), `make precheck`, `make test`, `make fmt`,
`make clippy`, `make check`. `make check` wymagał wąskich inline `nosemgrep`
dla Semgrep false positives na wewnętrznych pathach `state.json`, `index.json`
i atomic tempfile; follow-up commit: `ec4f74e`.

**Lessons.**
- Lock na pojedynczy `load` i pojedynczy `save` nie chroni transakcji. Jeśli
  decyzja dedup/watermark zapada między nimi, lock musi objąć całą sekcję.
- JSON state nie może mieć `unwrap_or_default()` na parse failure; default jest
  poprawny tylko dla missing file, nie dla corruption.
- Po zdjęciu locków wewnętrznych trzeba od razu sprawdzić poboczne CLI surface
  (`aicx state`), bo inaczej reset/info zostałyby poza nowym kontraktem.

**Related.** Area B Wave-B (B-2 + B-3) z
`/Users/silver/Downloads/bug-tracker-aicx.md` linie 645-1058 oraz
`/Users/silver/AI_notes/projects/aicx/reports/subagents/SUBAGENT_02_audit-area-B--20-05-2026.md`.

---

## 2026-05-20 — doctor truth: real embedder smoke + Skipped/NotConfigured severity (Area F P1+P2) · {commit-sha}

**Symptom.** `aicx doctor` was lying. It reported `Severity::Green` for embedder health just because a TCP connection could be opened, even if the embedder endpoint was actually broken or required auth. Also, it inflated warnings by defaulting `CheckResult::default()` to `Warning`, and showed Green for steer databases when the feature was compiled out.

**Fix.**
- Expanded `Severity` enum with `Unknown`, `Skipped`, and `NotConfigured` variants, changing the default to `Unknown` to prevent inflation of warnings.
- Upgraded the embedder probe from a TCP `HEAD` check to a real `POST /v1/embeddings` using a new `probe` helper in `aicx_embeddings::cloud`, validating the response shape and dimension.
- Hid the HTTP probe behind a new `--smoke` flag on `aicx doctor`. Without `--smoke`, `check_embedder_warmth` correctly reports `Severity::Skipped` and `check_semantic_health` stays cheap.
- Updated `check_steer_lance` and `check_steer_bm25` to return `Severity::NotConfigured` if the `lance` feature is disabled, rather than defaulting to Green. If the feature is enabled but no actual query is run, it returns `Severity::Skipped`.
- Bumped `DoctorReport` schema version to 2 via `#[serde(default)]` migrations.

**Touched.**
- `src/doctor.rs` — Severity enum, all checking logic, parsing legacy defaults.
- `src/main.rs` — Wires the `--smoke` flag into `DoctorOptions`.
- `src/embedder/mod.rs` & `crates/aicx-embeddings/src/cloud.rs` — Extracted and exposed the HTTP JSON probe logic.

**Related.** Area F Priority-4 truth-restoring fixes (F-P1-10, F-P1-11, F-P2-13, F-P2-14) from `docs/bug-tracker-aicx`.

---

## 2026-05-20 — markdown HTML escape + dynamic fence + CRLF normalization (Area C P2) · `fff5cc5`

**Symptom.** `write_formatted_message` w `src/output.rs` emitował user / agent
message body jako raw markdown / HTML. Single-line szło do `>` blockquote bez
HTML escape, multi-line z triple backtickami trafiało do literalnego
`<blockquote>` którego inner linie też nie były escapowane. Payloady typu
`<script>alert(1)</script>`, stray triple backticks, markdown link injection
i CRLF mogły zmieniać strukturę artefaktu albo wykonywać się w permissive
rendererach (GitHub, browser otwierający `.md` jako HTML).

**Root cause.** Markdown writer traktował message body jako safe markup.
`write_blockquote_with_code` echował linie verbatim a sam `<blockquote>`
zakładał że inner content jest trusted. Brak dynamicznego dobierania fence,
brak escape pass i brak normalizacji newline — dowolne wewnętrzne backticki
długości >= 3 mogły zamknąć otaczający markdown, dowolny raw `<script>`
przeżywał nietknięty, a `\r\n` przeżywał z powrotem do artefaktu.

**Fix.**
- Dodano trzy private helpery w `src/output.rs`:
  - `html_escape` (output.rs:699) — escapuje `& < > " '`, lustruje istniejące
    private kopie w `src/dashboard.rs:1020` i `src/reports_extractor.rs:1282`
    (brak wspólnego aicx helpera; zostawione spójnie z konwencją zamiast
    wprowadzać nowy util module mid-wave).
  - `dynamic_fence_for` (output.rs:710) — skanuje content szukając najdłuższego
    runu backticków, zwraca `max(3, longest+1)` backticków żeby wrapping fence
    nie mógł zostać zamknięty przedwcześnie przez inner content.
  - `normalize_newlines` (output.rs:728) — kolapsuje `\r\n` / lone `\r` do
    `\n`, zwraca `Cow` żeby unchanged input unikał alokacji.
- Przepisano `write_formatted_message` (output.rs:648): najpierw normalize
  newlines, potem routing — code-bearing → `write_blockquote_with_code`,
  single-line → markdown `>` blockquote z `html_escape`, multi-line plain →
  per-line `>` blockquote z `html_escape`.
- Przepisano `write_blockquote_with_code` (output.rs:684): wraps body inside
  `<blockquote>` + outer dynamic fence żeby inner backticki nie mogły się
  wydostać i żeby dowolny inner HTML / markdown renderował się jako inert
  code text.
- JSON writery (output.rs:346, output.rs:373, output.rs:831) nietknięte —
  audit Area C potwierdził że `serde_json` jest RFC-compliant; ten fix tylko
  dodaje regresyjne testy wokół tego kontraktu.

**Touched.**
- `src/output.rs` — helpery, markdown write paths, 8 nowych testów.

**Tests.** 8 nowych testów w `src/output.rs::tests` (30/30 total w module):
`test_html_escape_neutralizes_script_payload`,
`test_stray_triple_backtick_does_not_break_out`,
`test_link_injection_does_not_become_active_link`,
`test_crlf_normalized_to_lf`, `test_dynamic_fence_avoids_collision`,
`test_json_escapes_control_chars`, `test_json_handles_bom_in_message`,
`test_json_invalid_input_rejected_upstream`. Zielone: `cargo test --package
aicx --lib output::` (30/30), `make precheck`, `make test`, `make clippy`,
`make fmt`.

**Lessons.**
- Trust boundary dla `output.rs` to message body, nie renderer. Defensive
  escape at write time jest tańszy niż audytowanie każdego downstream
  renderera pod kątem raw-HTML permissywności.
- Outer fence length musi być dłuższy od **najdłuższego runu** inner
  backticków, nie po prostu count occurrences — trzy osobne 3-tick runy
  i jeden 4-tick run oba wymagają takiego samego 5-tick outer fence.
- `Cow` dla `normalize_newlines` trzyma zero-CR case alokacja-free; większość
  transkryptów Claude / Codex / Gemini już używa LF, więc borrow path to hot
  path.
- Parallel-wave Living Tree etiquette: W2-C output.rs cut był autorski na
  swoim branchu ale wylądował wewnątrz sibling commit `fff5cc5` bo tamta fala
  stage'owała szersze working tree niż jej scope. Substrate boundary notowane
  poniżej w sekcji Living Tree; kod w drzewie jest poprawny i gaty zielone.

**Related.** Area C Priority-2 (2.1–2.5) z
`/Users/silver/Downloads/bug-tracker-aicx.md` linie 1278–1333 oraz
`/Users/silver/AI_notes/projects/aicx/reports/subagents/SUBAGENT_03_audit-area-C--20-05-2026.md`.

---

## 2026-05-20 — sources.rs diagnostics bundle + BufReader caps (Area A + C P3) · `{commit-sha}`

**Symptom.** Ekstraktory w `src/sources.rs` miały kilka cichych ścieżek
utraty prawdy: dropowały wpisy przy nieznanym `msg_type`, gubiły content gdy
dało się go odzyskać z `payload.role`, ignorowały drift `sessionId`, nie
raportowały części błędnych timestampów, mieszały formaty Codex history/session
bez diagnostyki, brały Junie session id z najbliższego katalogu zamiast
ancestor `session-*`, oraz czytały JSONL przez nieograniczone `BufReader`
linie.

**Root cause.** Każdy extractor rozwijał własne "best effort" zachowanie bez
wspólnego kontraktu diagnostycznego. Codex miał już `CodexSessionWarning`,
ale Claude/Gemini/Junie nie miały równoważnej powierzchni warningów, a część
parserów w razie nieznanego shape robiła `continue` zamiast zachować payload
jako jawny systemowy artefakt.

**Fix.**
- Dodano per-extractor warning enums:
  `ClaudeSessionWarning`, `GeminiSessionWarning`, `JunieSessionWarning` oraz
  rozszerzono `CodexSessionWarning` o mixed format / oversized line.
- Ujednolicono timestamp parsing przez `parse_rfc3339_or_naive_utc`: RFC3339
  (`Z`, offset, fractional) plus naive ISO traktowany jako UTC; błędne wartości
  idą do diagnostyki zamiast znikać po cichu.
- Claude/Gemini wybierają pierwszy niepusty `sessionId`, raportują drift i
  missing id. Claude history używa bezpiecznego
  `DateTime::<Utc>::from_timestamp_millis`.
- Codex/Gemini/Junie zachowują nieznane albo systemowe eventy jako
  `FrameKind::SystemNote`; `payload.role` może uratować tool/tool_result
  content zanim event trafi do fallbacku.
- Codex mixed history/session JSONL wykrywa oba formaty per line i emituje
  `MixedFormat`, zamiast wybierać jeden format i tracić resztę.
- Junie session id chodzi po ancestorach szukając `session-*`; brak takiego
  katalogu dostaje deterministyczne `unknown-<path-hash>` i warning.
- Project filter w `sources.rs` przeszedł na strict path-segment matching
  zgodny z `store.rs`, bez substring false positive typu `vista` vs
  `vista-portal`.
- JSONL readers dostały `MAX_LINE_BYTES = 8 MiB` i `read_line_limited`, z
  `OversizedLine` warningiem i drainem do kolejnej linii.

**Touched.**
- `src/sources.rs` — extractor diagnostics, timestamp fallback, content
  preservation, mixed-format handling, strict project filter, Junie ancestor
  session id, bounded line reads.
- `src/sources/tests.rs` — 33 nowe testy wokół Area A i Area C P3.
- `crates/aicx-parser/src/timeline.rs` — nowy `FrameKind::SystemNote`.

**Tests.** Zielone:
`cargo test --package aicx --lib sources::` (111/111),
`make precheck`, `make clippy`, `cargo fmt --all -- --check`.
Repo-wide `make test` / `make check` zatrzymały się na out-of-scope W3-F
regresji w `src/main.rs::tests::serve_help_prefers_http_name_and_stays_compact`;
`src/lib.rs` część testów przeszła 456/456.

**Lessons.**
- Parser nie powinien traktować nieznanego shape jako zgody na utratę contentu;
  najbezpieczniejszym fallbackiem jest zachowany `SystemNote` plus warning.
- Bounded line read musi drainować oversized line przed kolejnym `read_line`,
  inaczej limit zmienia parser w generator sztucznych fragmentów.
- Path filters w source extraction muszą być segmentowe, bo substring matching
  wygląda wygodnie tylko do pierwszego repo typu `vista-portal`.

**Related.** Area A (A-1..A-25) z
`/Users/silver/Downloads/bug-tracker-aicx.md` linie 15-643 oraz Area C P3
linie 1335-1396; audyty
`/Users/silver/AI_notes/projects/aicx/reports/subagents/SUBAGENT_01_audit-area-A--20-05-2026.md`
i
`/Users/silver/AI_notes/projects/aicx/reports/subagents/SUBAGENT_03_audit-area-C--20-05-2026.md`.
- B-6: Switched deduplication to BLAKE3-128 collision-resistant algorithm.
- B-7: Bumped default state lock timeout to 60s and plumbed --lock-timeout flag.
