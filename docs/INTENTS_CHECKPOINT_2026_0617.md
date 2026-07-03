# AICX Intents Checkpoint - 2026-06-17

Checkpoint dla branchu `fix/aicx-intents`.

Cel tej rundy: zrozumiec i urealnic `aicx intents` jako semantyczny parser
operator/user truth, szczegolnie wokol granic `intent`, `task`, `decision`,
`outcome`, `assumption` i `commitment`, oraz dodac sciezke importu obcych plikow
Markdown przez `operator-md`.

## Stan brancha

> UPDATE 2026-06-20: ponizszy opis byl prawdziwy w momencie checkpointu
> (Runda I, sesja codex 019ed7c6, dirty worktree na `fix/aicx-intents`).
> AKTUALNY stan runtime: cala Runda I jest scommitowana i wypchnieta na
> `agent/aicx-search-quality-defaults` (HEAD `42ba27d`, 25 commitow ahead
> `develop`, worktree CZYSTY, brak PR). Commity Rundy I: `4768a77` (taxonomy
> Task/Commitment), `9f742ad`, `0192e01`, `76c779e`. Branch `fix/aicx-intents`
> to dzis tylko baza `release/v0.9.3` (10401e0), upstream gone.

Historyczny stan w momencie checkpointu (Runda I):

- Branch: `fix/aicx-intents`.
- Upstream: `origin/fix/aicx-intents [gone]`.
- Nic nie bylo commitowane ani pushowane w tej rundzie.
- Worktree jest dirty i zawiera zarowno runtime zmiany, jak i nowe docs/test
  fixtures.

## Główne artefakty

- `docs/INTENTS_CLASSIFICATION_RULES.md`
  - opisuje obecne runtime rules classifiera
  - pokazuje 11 internal `EntryType` i mapowanie do 4 widocznych bucketow CLI
  - zawiera miejsca edycji markerow/slownikow
- `docs/INTENTS_CORE_ONTOLOGY.md`
  - opisuje target ontology: `intent`, `task`, `decision`, `outcome`,
    `commitment`, `assumption`, `claim`, `insight`, `argue`
  - zawiera golden examples i semantyczne kryteria
- `tests/fixtures/intents_core_ontology_goldens.json`
  - 15 golden examples dla core ontology
- `docs/INTENTS_CHECKPOINT_2026_0617.md`
  - ten checkpoint

## Co zostało zmienione w classifierze

Internal taxonomy w `crates/aicx-parser/src/types.rs` ma teraz 11 typow:

- `Intent`
- `Task`
- `Commitment`
- `Why`
- `Argue`
- `Decision`
- `Assumption`
- `Outcome`
- `Result`
- `Question`
- `Insight`

Mapowanie do widocznych `aicx intents --kind ...` bucketow:

- `Decision` -> `decision`
- `Task` -> `task`
- `Intent`, `Question`, `Why` -> `intent`
- `Outcome`, `Result` -> `outcome`
- `Commitment`, `Assumption`, `Insight`, `Argue` -> dropped z widocznego CLI

Najwazniejsze poprawki semantyczne:

- `musimy ...`, `trzeba ...`, `nie moze byc ...` to zwykle `intent`, nie
  automatycznie `decision`.
- Policy/default/scope language typu `tylko przez`, `bez zgadywania`,
  `canonical`, `od teraz`, `nie fixujemy` nadal moze byc `decision`.
- Pytania sa klasyfikowane przed broad policy markers, wiec
  `Why keep the canonical index path...?` zostaje `question` -> widoczny
  `intent`, a nie `decision`.
- `musi miec` / `musi mieć` zostalo zachowane jako durable property/constraint
  marker dla `decision`, np. `Material ... musi miec source_hash`.
- `task:` / `todo:` / `zadanie:` staja sie `Task`, nie `Intent`.
- Bare checkbox `[ ] ...` / `[x] ...` moze byc raw semantic `Task` nawet bez
  bullet prefixu.
- Konkretny imperatyw usera, np. `stworz prosze plik ...`, moze byc `Task`.
- Checklist tasks nadal maja osobny lifecycle: open/closed finalization i tylko
  open tasks przezywaja. Raw semantic tasks sa zwyklymi candidates.
- `zrobie to zaraz` / `promise:` / `commitment:` / `obietnica:` trafia w
  internal `Commitment`; nie jest outcome ani task.
- `zakladam` bez polskich znakow dziala jako `Assumption`.
- Natural-language outcomes zostaly rozszerzone ostroznie o:
  - completed file/artifact reports typu `plik docs/X.md zostal dodany`
  - observed count reports typu `batch 10 plikow dal 66 records`

## Operator-md import

Dodana jest sciezka `operator-md` dla obcych plikow Markdown.

Warstwa celu:

- obcy `.md` nie musi udawac natywnej sesji agentowej
- mozna nadac mu minimum metadanych i przepuscic przez canonical chunk flow
- import nie zgaduje agresywnie `cwd`; projekt/cwd powinien byc jawny albo
  swiadomie ustawiony przez operatora
- obce md moga trafiac do projektu, jesli rozmowa dotyczy konkretnego repo,
  albo pozostac osobnym importem, jesli operator nie chce mieszac tego z
  sesjami agentowymi

Główne pliki:

- `src/sources/providers/operator_markdown.rs`
- `src/sources/providers/mod.rs`
- `src/sources/mod.rs`
- `src/main.rs`
- `tests/operator_md_ingest.rs`

## Slowniki / listy markerow do edycji

Najwazniejsze miejsca:

- `src/intents.rs`
  - `POLICY_MARKERS`
  - `REQUIREMENT_MARKERS`
  - `TASK_ACTION_HEADS`
  - `COMMITMENT_HEADS`
  - `COMPLETION_MARKERS` w `looks_like_completion_outcome_line`
  - `looks_like_observed_count_outcome_line`
  - `ASSUMPTION_MARKERS`
  - `QUESTION_MARKERS`
  - `WHY_MARKERS`
  - `ARGUE_MARKERS`
  - `INSIGHT_MARKERS`
- `crates/aicx-parser/src/chunker.rs`
  - `INTENT_KEYWORDS`
- `src/sources/shared/conversation.rs`
  - `TYPED_DIRECTIVE_HEAD_MARKERS`

User dictionary/calibration layer byl omawiany, ale zostal celowo odlozony.
Wniosek: slownik uzytkownika ma byc dodatkiem i kalibracja, nie ratunkiem dla
core ontology.

## Golden audit

Golden audit:

```bash
cargo test -q core_ontology_goldens_match_target_semantics -- --ignored --nocapture
```

Stan teraz: zielony, 15/15 examples matchuja target semantics.

Wazne: test jest nadal `#[ignore]`, bo to jest audit/contract tool do
uruchamiania swiadomie, nie zwykly unit test w kazdym szybkim przebiegu.

## Verification

Ostatni znany stan po naprawieniu regresji:

```bash
cargo fmt
cargo test -q classifier:: -- --nocapture
cargo test -q core_ontology_goldens_match_target_semantics -- --ignored --nocapture
cargo test -q --test operator_md_ingest -- --nocapture
cargo test -q operator_md -- --nocapture
cargo test -q
git diff --check
```

Wynik:

- `cargo test -q` zakonczyl sie exit code `0`.
- Glowny lib target: `791 passed; 0 failed; 1 ignored`.
- Pelny suite przeszedl wszystkie kolejne targets.
- `git diff --check` czysty.

Uwaga: podczas pelnego suite pojawia sie testowy log:

```text
steer_sync FAILED
cause: Lance index corrupted
recover: aicx doctor --rebuild-steer-index
```

To bylo w zielonym przebiegu i wyglada na kontrolowany failure-path test, nie
realny koniec suite. Exit code pelnego `cargo test -q` byl `0`.

## Naprawione regresje złapane przez full suite

Pelny `cargo test -q` po pierwszym semantycznym cutcie zlapal regresje. Zostaly
naprawione przed checkpointem:

- `Zadanie:` stare testy oczekiwaly `intent`; zaktualizowano je do nowej prawdy
  `task`.
- `Why ... canonical ...?` bylo blednie klasyfikowane jako `decision`; pytania
  ida teraz przed broad policy markers.
- `musi miec source_hash` zniknelo z decision lane; przywrocono narrow durable
  property marker `musi miec` / `musi mieć`.
- Dwa store tests zakladaly, ze brak `AICX_HOME` zawsze znaczy `~/.aicx`; obecny
  kontrakt uwzglednia bootstrap config `[storage].home`. Testy poprawiono na
  pure `resolve_aicx_home_from(...)` z temp home bez bootstrap configu.

## Loctree note

Loctree MCP w tej sesji reklamowal `find`, ale callable surface w Codex
pokazywal tylko m.in. `focus`, `slice`, `context`, `tree`, `impact`,
`repo_view`, `prism`, `suppressions`.

Fallback:

```bash
loct occurrences Commitment
```

Wynik: jedyne wystapienie `Commitment` przed dodaniem symbolu bylo w
`docs/INTENTS_CORE_ONTOLOGY.md`.

Hak dopisany do:

```text
/Users/silver/.vibecrafted/loctree/loctree-fail.md
```

## Known Open Findings (otwarty backlog) — przywrocone 2026-06-20

Te findingi byly POTWIERDZONE w sesji 019ed7c6 (kod + runtime), ale wypadly z
pierwotnej wersji checkpointu. Przywrocone, bo checkpoint, ktory gubi ustalenia,
sam powiela problem AICX, ktory naprawiamy. Wszystkie nadal OTWARTE
(weryfikacja kontraktu wymagana wzgledem aktualnego HEAD przed napraweniem):

- **CLI `--since YYYY-MM-DD` dziala jak exact-day**, mimo ze help mowi
  lower-bound. Zrodlo: `parse_date_filter` zwraca `(Some(day), Some(day))` dla
  single date (`src/main.rs` okolice :7224 w 019ed7c6 — zweryfikowac linie w HEAD).
  Runtime dowod: `--since 2026-06-01` => `results: 0`, a `--since 2026-06-01..`
  przy tym samym all-time scan => wynik.
- **CLI i MCP maja rozna semantyke dat.** MCP `aicx_intents` przyjmuje `since`
  jako lower-bound i ma default `limit=20` cap `500` (`src/mcp.rs` okolice :449);
  CLI single-date robi exact-day. Ten sam `since` daje rozny obraz swiata.
- **Core project filtering uzywa substringowego `contains`.** Extractor:
  `file.project.to_ascii_lowercase().contains(project)` (`src/intents.rs`
  okolice :284). CLI probuje resolve do canonical slugow (`src/main.rs` ~:4198),
  ale MCP idzie blizej core scizki => ryzyko powrotu substring-leak.
- **`--limit` ogranicza tylko display, nie skan.** Ekstrakcja najpierw skanuje
  caly zakres, limit dziala dopiero display-time. `--limit 3` i tak skanuje
  setki chunkow (potwierdzone: `oracle_status` 46 scanned / 20 candidate / 3 out).

## Round II — pipeline truth (plan, NIE wykonany)

Diagnoza po review packa (operator, 2026-06-19): zielony golden audit 15/15
NIE oznacza poprawnego E2E. Runda I naprawila line classifier, ale przed nim
dziala osobny silnik `[signals]`, ktory ma za duzy autorytet. Osie Rundy II:

1. **Unifikacja semantyki signal vs raw.** `[signals]` nie moga omijac tej samej
   ontologii — albo przechodza przez wspolny semantic gate, albo dostaja
   `source=signals` + rewalidacje przed wejsciem do `IntentRecord`. Dane z packa:
   ~93/136 outcomes (~70%) pochodzilo z `[signals]`, ktorych golden audit
   classifiera bezposrednio nie testuje.
2. **Quote / negation / historical-context awareness.** False positives w packu:
   kod/logi z `default` (np. `DEFAULT_KEYWORDS_PATH`) -> decision; tekst commita
   `Add...` -> task; negacja `must not create...` -> task. Z 5 taskow tylko 2
   realne. To nie jest problem kolejnej listy czasownikow — potrzebne rozpoznanie
   cytatu, referencji historycznej, negacji i roli tekstu w dokumencie.
3. **Provenance jako struktura, nie tekst.** Wszystkie 462 rekordy w packu mialy
   pusty `project` (record dostawal filtr z zapytania zamiast `file.project`).
   `source_file`/`source_format` byly w tresci wiadomosci, nie w sidecarze.
   Brak `import_id`, hash, filtra po imporcie. Query filter nie moze byc zrodlem
   prawdy dla `record.project`.
4. **Task lifecycle decision (produktowa).** Checklist task ma lifecycle
   open/closed; natural-language task to pojedynczy rekord bez zamkniecia (moze
   wygladac na wiecznie otwarty). Dwa znaczenia sklejone jedna etykieta. Decyzja:
   `task` = tracked work item czy actionable mention? Ewentualnie osobny status
   `work_item_candidate` albo subtype `actionable=true` na `intent`.
5. **Foreign-MD importer jako auditowalny import.** Daty rozmow spłaszczane do
   daty eksportu/mtime (niszczy os czasu wielotygodniowej rozmowy). Attach
   resolver zgaduje checkout i moze wybrac zly klon. `frontmatter project` dzis
   pomaga znalezc cwd, ale nie ustawia bezposrednio canonical projektu; brak
   walidacji konfliktu `frontmatter repo A` vs `cwd repo B`. Potrzebny stabilny
   `import_id`/hash, zeby ten sam plik nie wchodzil ponownie jako nowy.

Review-pack reproducibility (uwaga reviewera): nastepny pack ma miec base commit
jawnie w README/manifest, `git bundle` albo pelny checkout, niepusty
`verification/` z logami `cargo test -q` / `git diff --check` / `cargo fmt --check`,
komende odtworzeniowa od zera i osobny `REVIEW_NOTES.md` z known limitations.

Kolejnosc rundy II: (1) checkpoint/backlog repair [TEN DOKUMENT], (2) E2E golden
fixtures (foreign md -> canonical chunk -> signals+raw -> final records),
(3) signal revalidation gate, (4) provenance repair, (5) task semantics decision.

## Co dalej

Najbardziej sensowny kolejny krok:

1. Uruchomic `operator-md` na batchu prawdziwych plikow z eksportow ChatGPT.
2. Porownac output `intents`, `tasks`, `outcomes`, dropped internal types.
3. Dopisac kolejne golden examples dla falszywych klasyfikacji.
4. Dopiero potem rozszerzac markery, nie przed realnym corpus feedbackiem.

Kolejne tematy produktowe do decyzji:

- Czy `Commitment`, `Assumption`, `Insight`, `Argue` maja dostac widoczny CLI
  output, osobny command, JSON-only lane, czy zostac internal.
- Czy raw semantic `Task` ma miec lifecycle jak checklist tasks, czy zostaje
  point-in-time candidate.
- Czy user dictionary/calibration ma byc plikiem config, repo-local profile,
  operator-side profile, czy pozniejsza warstwa w UI.

## Szybki restart pracy

Po powrocie:

```bash
cd /Users/silver/Git/aicx
git status --short --branch
cargo test -q classifier:: -- --nocapture
cargo test -q core_ontology_goldens_match_target_semantics -- --ignored --nocapture
```

Jesli trzeba sprawdzic calosc:

```bash
cargo test -q
git diff --check
```

