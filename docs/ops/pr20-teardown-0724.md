# PR #20 — Teardown na atomy

**Repo:** `Loctree/aicx` · **PR #20** "Enhance CLI, intents, and session workflows with new features and fixes"
**Stan:** MERGED · `release/v0.9.3` → `develop` · merge commit `7b293c1` (single-parent) · 2026-06-17 22:52 CET (Szowesgad)
**Statystyka GitHub:** +345 / −455, 16 plików, ~25 commitów na liście
**Źródła:** 3× równoległy research (git/GitHub · `~/.aicx` store · `~/.vibecrafted` control-plane), cross-weryfikowane.

---

## 0. TL;DR — jedno zdanie prawdy

PR #20 to **domykający merge pociągu releasowego 0.9.3**: jego *netto*-diff jest wąski (**Grok jako first-class agent + przełącznik Windows GNU→MSVC + przepisany README 474→167 + demo-transcripty**), ale siedzi na szczycie **trzech równoległych linii dyspozycji wieloagentowej**, których większość (intents Lane 1–5, hardening „haki", release 0.9.3) wpłynęła do `develop` już wcześniej przez **PR #19** — dlatego lista commitów PR #20 **zawyża scope ~3×**.

---

## 1. Iluzja zakresu — najważniejsza rzecz do zrozumienia

`gh pr view 20 --commits` wypisuje ~25 commitów. **21 z nich nie dołożyło ani jednej linii netto do tego PR** — wpłynęły do `develop` przez **PR #19** ("Implement intent stages, fix CLI SIGPIPE handling, and release 0.9.3"), którego merge (`99ea64d`) jest jedynym rodzicem `7b293c1`.

**Jedyne źródło prawdy o zakresie PR #20 to diff `99ea64d..7b293c1`, nie lista commitów.** Kto audytuje po commitach — masowo przeszacuje.

Co REALNIE weszło w PR #20:

| Blok | Agent | Co |
|---|---|---|
| Grok extract-agent (klon codex) | `grok` (`448e4fd`, z `Authored-By: grok`) | first-class ekstraktor |
| Grok session workflows | `grok` (`f2ae800`, `10401e0e` — **bez trailera Authored-By**) | filtry CLI, current-session, diagnostics |
| README rewrite + Windows MSVC + demo | *nieatrybuowane* (spakowane w `10401e0e`) | commit message NIE opisuje własnego diffa |

---

## 2. Atomy kodu (realny diff)

- **`src/main.rs`** (~+46/−23): literały listy agentów `[claude,codex,gemini,junie]` → `+grok` w 5 clap value_parserach/docstringach; `GROK_SESSION_ID`/`GROK_THREAD_ID` do `CURRENT_SESSION_ENV_KEYS`; nowa `current_session_from_grok_active()` (czyta `~/.grok/active_sessions.json`, matchuje `cwd`); **`LEGACY_ALL_WATERMARK_KEY`** `"claude+codex+gemini+junie"` → `"...+grok+codescribe"` (behawioralne — zmienia klucz cache decydujący incremental-vs-full rescan).
- **`src/sessions.rs`** (+15/−1): nowa `discover_grok_sessions()` — woła `discover_codex_sessions()` i relabeluje `agent="grok"`; `.grok` root przestaje być mislabelowany jako `codex`.
- **`src/diagnostics.rs`** (+1/−1): `EXTRACTOR_ORDER` += `"grok"`.
- **`src/main/tests.rs`** (+7/−1): dwie asercje help-textu poluzowane (tolerują stare i nowe brzmienie).
- **`tests/runtime_cli_store_contract.rs`** (+1/−1): oczekiwany watermark → `"...+grok+codescribe:all"`.
- **`install.sh`** (1 linia): `*windows-gnu` → `*windows*` w case rozszerzeń archiwum.
- **`distribution/npm/verify-metadata.mjs`** + **3× `postinstall.js`**: assetTriple `x86_64-pc-windows-gnu` → `...-msvc`, filename `windows-gnu-slim` → `windows-msvc-slim`.
- **`.github/workflows/merge-queue-gate.yml`** + **`release.yml`**: usunięcie `TARGET=...windows-gnu` + Strawberry Perl PATH + `rustup target add`; zastąpione `TARGET=x86_64-pc-windows-msvc` (MSVC = default runnera).
- **3 nowe pliki demo** (`quantum-reasoning.txt`, `pivot-chwila.md`, `this-pivot-chat.jsonl`): fixture-transcripty jako stress-test silnika intencji (5 lane / 9 typów).

**README 474→167:** z dokumentu extractor-centric (`# AI Contexters`, tabela taksonomii 9-typów, indeks docs, sekcja Notes o redakcji sekretów) → operator-centric (`# aicx`, „The Model"/„Intents Engine", Command Surface z żywego binarki, Grok jako supported agent, Philosophy). **Uwaga: sekcja Notes (disclosure o redakcji sekretów, wymóg PATH, gwarancja no-silent-download) NIE przeniosła się** — treść operator-safety wypadła.

---

## 3. Orkiestracja za tym — trzy linie dyspozycji (skąd się wziął 0.9.3)

Cała fala 0.9.3 zbudowana na jednym Living Tree (branch `ci/portable-sha256` → `release/v0.9.3`), udokumentowana w `~/.vibecrafted/artifacts/.../2026_06{12,14,15}/`.

### Linia „W" — triple line (06-12, dispatcher `8ab9f0dc`→recovery `2f8125fe`)
| Cut | Agent | Zadanie | Commit |
|---|---|---|---|
| W1-A chunkref-contract | codex | typed `ChunkRefSpec` resolver | sweep |
| **W1-B lane3-evidence** | **gemini** | `EvidenceRecord` + `audit_claims_against_evidence` (deterministyczny, zero I/O) | `5d59e87` |
| W1-C release-092-recon | gemini (report-only) | recon padłego Windows CI v0.9.2 (`missing shasum`) → fix atlas `sha256_file()` | report |
| W2-A loctree-consumer | codex | slim library profile (0× lancedb/llama/rmcp/axum) | `eb39265` |
| **W2-B lane4-fractures** | agy/mac | `detect_contract_fractures` (contradicted/unsupported/orphaned) | `b241db3` |
| **W3-A lane5-clarify** | agy/mac | `generate_clarify → Vec<ClarifyQuestion>` (deterministyczny sort, cap 5) | `96e6f29` |

Zależność: W1-B→W2-B→W3-A sekwencyjne (współdzielą `src/intents/schema.rs`).

### Linia „aicx-haki" — twin (06-12, dispatcher `2f8125fe`, ten sam tree)
| Cut | Agent | Zadanie | Commit |
|---|---|---|---|
| HK-A sigpipe-stdout | codex | SIGPIPE-default (repro: `aicx intents ... \| head` → panic `stdio.rs:1165`) | `5f0b505` |
| HK-B pipeline-truth | codex | „freshness lie" — `pending=0` przy 21h staleness → `sessions_newer_than_chunks` + catch-up | `87da91a` |
| **HK-C intents-quality** | agy | voice-transcript provenance `[voice]` + garble gate (repro: STT „od Arozet… kadensy") | `caef5f8` |
| HK-D config-root-truth | codex | print *resolved* AICX_HOME, nie hardcoded | `57c3d30` |
| HK-E project-identity | codex | `-p` jak `sessions list` + „did you mean" (bug Moniki #1) | `4b0aa8f` |
| HK-F filter-semantics | agy | `--unresolved-mode session\|intent` + `--min-confidence` (bug Moniki #2) | `b0c6f63` |

### Linia „aicx-intents-unify" (06-14/15, dispatcher `610fcd17`, codex solo)
Wywołana bug-reportami (operator na loctree-suite + „Mixa" na md-radar). Trzy sekwencyjne cuty na `src/intents.rs` (hard-overlap → zakaz paraleli): `76cc1c8` (9-type surface), `7bd634e` (stop charter re-ingest multiplying), `163ffb5` (gate tool/agent lines z OUTCOME).

### Loose ends (bez artefaktu dispatchu — interaktywne/manualne)
`[maciej/vc-frame]` (operator bezpośrednio, `Authored-By: maciej`), `[codex/workflow]` hardening, i **grok ×3 (06-15)** — onboarding Groka **bez śladu DRIVER/SCAFFOLD**, robiony w surowej sesji interaktywnej.

---

## 4. Intencja — po co to wszystko

Dwie warstwy „dlaczego":
1. **Kręgosłup epistemiczny intencji (Lane 1–5):** od kandydatów (Lane 1) przez evidence-audit (3), contract-fractures (4), do clarify-questions (5) — cała maszyna do odróżniania *twierdzenia* od *dowodu* w sesjach agentów. To był główny produktowy cel 0.9.3.
2. **„Haki" — dogfood-hardening:** rzeczy które bolały operatora i Monikę w realnym użyciu (SIGPIPE crash, kłamiący `pending=0`, garble ze STT łapany jako Intent, false-empty `-p`, `--unresolved` zwracające 0 z 292/1150).

Perła z JOURNALa (tłumaczy posplataną atrybucję commitów): *„przy 4 traktorach na jednym drzewie commit przestaje być jednostką atrybucji — jednostką prawdy jest CONTENT na HEAD + verifier"*. Sweepy `git add -A` dispatchera mieszały pliki workerów między commitami.

---

## 5. Merge-hygiene i ryzyko — wzorzec który wrócił 0724

- **Review: tylko boty** — `gemini-code-assist` (COMMENTED, generyk) + `copilot` (COMMENTED, jeden konkretny flag). **Zero human review.**
- **Checks: tylko `linux-self-hosted` + `macos-self-hosted` (SUCCESS).** **Żaden windowsowy run CI się nie odpalił** — a PR **przełączył Windows na MSVC**. Największa dystrybucyjna zmiana PR-a **nie została zwalidowana przez CI** przed merge.
- **Tempo:** 28 min od open do merge, ~21 min po ostatnim komentarzu bota. Bez czekania na człowieka.
- **To ten sam wzorzec co incydent 0724:** szybki merge, bot-only, brak windowsowej walidacji. PR #20 jest wczesnym prawzorem tej samej dziury procesowej.

### Otwarty defekt (do dziś)
**`aicx-win32-x64-gnu` kłamie:** nazwa pakietu npm, klucz optionalDependency i katalog platform-package nadal mówią `-gnu`, a shipują binarkę **MSVC**. Copilot to flagnął w review PR #20 — **nienaprawione**.

---

## 6. Luki w zapisie (gdzie historia zniknęła)

1. **`~/.aicx/catalog`** nie ma wpisów **06-12→06-21** dla tego repo — realna praca (06-12) nie siedzi jako raw-extract; żyje tylko w artefaktach vibecrafted.
2. **`~/.aicx/aicx-problems.md`** — **zero wpisów z czerwca** (najstarszy 2026-07-23). Problemy tej ery (SIGPIPE, freshness-lie, garble) logowane tylko w JOURNAL, nie w problem-logu.
3. **`~/.vibecrafted/control_plane`** — dziura **06-16→06-22** w `events_archive` (potwierdzone binary-searchem po timestampach, nie samym grepem). Sam akt merge `7b293c1` **nie ma śladu** — zgodne z doktryną „merge = guzik operatora, nigdy automat".
4. **`2026_0617` w artefaktach = red herring** (niezwiązany swarm o quantum computing, status: failed).
5. **Grok onboarding** (06-15) — brak DRIVER/SCAFFOLD; interaktywny.

---

## 7. Najostrzejsze wnioski

1. **Commit-lista ≠ scope.** Squash single-parent czyni `gh pr view --commits` mylącym; prawda to diff `99ea64d..7b293c1`. (Moja wcześniejsza kotwica „main.rs = SIGPIPE" była błędna — to PR #19. Klasyczny cutoffflu, złapany.)
2. **Zmiana dystrybucyjna (Windows MSVC) wjechała bez windowsowego CI, bot-only review, w 28 min** — prawzór incydentu 0724. To nie pech, to wzorzec procesowy.
3. **`win32-x64-gnu` nazewnictwo kłamie o zawartości** — otwarty defekt installability, flagnięty przez bota, nienaprawiony.
4. **README stracił operator-safety disclosure** (redakcja sekretów, no-silent-download) przy rewrite — regresja treści, nie tylko forma.
5. **Zapis incydentów tej ery jest dziurawy** w trzech warstwach (catalog, problem-log, control-plane) — jedyna pełna prawda żyje w `~/.vibecrafted/artifacts/.../plans/{DRIVER,JOURNAL}` + chronicle. To argument za tym, żeby JOURNAL/reports traktować jako kanoniczny zapis, bo telemetria i store mają luki.

### Rekomendowane follow-upy
- [ ] Naprawić `win32-x64-gnu` → spójna nazwa MSVC (npm key + dir + assetTriple) — installability.
- [ ] Wymusić windowsowy kontekst CI jako required (łączy się z patchem rulesetu z 0724).
- [ ] Przywrócić operator-safety Notes do README (redakcja sekretów, PATH, no-silent-download).
- [ ] Hak loctree: MCP advertised `find`, a Codex-surface go nie eksponował (`loctree-fail.md:6295`, 06-17) — zgłosić maintainerom.
