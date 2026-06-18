# AICX Intents Checkpoint - 2026-06-17

Checkpoint dla branchu `fix/aicx-intents`.

Cel tej rundy: zrozumiec i urealnic `aicx intents` jako semantyczny parser
operator/user truth, szczegolnie wokol granic `intent`, `task`, `decision`,
`outcome`, `assumption` i `commitment`, oraz dodac sciezke importu obcych plikow
Markdown przez `operator-md`.

## Stan brancha

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

