# Golden questions — search-quality (v2)

Zestaw realnych pytań do regresji jakości retrievalu (`aicx eval search-quality`).
Pytania celowo nieidealne: literówki, skróty, miks PL/EN — tak naprawdę pytam.

Ugruntowane w korpusie `tb-out-anchor-v4-20260619-121218` (spotlight.md per sesja).
Maszynowy odpowiednik tego pliku: `search_quality_seed.toml` (te same `id`).

## Schema / semantyka eval

Każde pytanie:

- `id` — stabilny identyfikator (nie zmienia się przy reorderowaniu).
- `scope` — projekt.
- `type`:
  - `evidence` — pytanie szuka konkretnego dowodu; mierzymy czy trafna sesja/chunk
    jest w topie.
  - `ask_answer` — pytanie bardziej "wyjaśnij" niż "znajdź". **Eval mierzy trafność
    evidence (czy wskazał właściwą sesję/chunk), NIE jakość gotowej narracji.**
- `Dobry wynik` / `Zły wynik` — opis trafienia/pudła (human).
- `anchors` — lista sesji gdzie żyje odpowiedź. Każdy anchor ma:
  - `map_id` — folder sesji.
  - `expected_terms` — tokeny, które trafny top-hit musi zawierać; **sprawdzane w
    treści `.md` (body prose), nie w `.meta.json`**. Domyślnie `terms_match = any`
    (wystarczy 1 token), chyba że zaznaczono `all`.
- `anchors_match` (tylko gdy >1 anchor): `any_of` (wystarczy trafić jeden) lub
  `all_of` (trzeba trafić wszystkie).

Zasada sekretów: żadnych fragmentów kluczy w pytaniach ani w `expected_terms` —
tylko nazwy zmiennych/lokalizacje i instrukcja rotacji.

> Uwaga do v1→v2: usunięto pytania o vista (brak dobrych źródeł w tych plikach),
> w tym dawne #16 "historia transkrypcji" — frazy `Audio pipeline audit` /
> `Transcript Ownership Refactor` żyją w korpusie tylko jako jedna gęsta linia
> meta-triage pamięci (cytat z `vista-claude/MEMORY.md`), więc grep je znajduje,
> ale retrievalny chunk `.md` jest słaby. To NIE jest dobry in-corpus case → wycięte.
> Zakres v2: aicx + Pensieve, Agent-blackbox, ScreenScribe, Loctree, md-radar,
> transcript-builder, vibecrafted.

---

## aicx — retrieval / index / intents

### `aicx-all-bucket`
type: evidence · scope: aicx
Pytanie: wtf jest z tym `_all` bucketem i czemu `aicx search -p vista` nie chodzi a `_all` chodzi?
Dobry wynik: `_all` = zbiorczy indeks całego store'a (`indexed/_all/embeddings.ndjson`), tryb projektowy oczekuje osobnego bucketu (`indexed/vetcoders_vista`) którego nie było.
Zły wynik: sama składnia `aicx search` albo surowy `index_status` bez różnicy `_all` vs project.
anchors: codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`_all`, `indexed/vetcoders_vista`, `embeddings.ndjson`, `bucket`]

### `aicx-sztudio-vs-silver`
type: ask_answer · scope: aicx
Pytanie: czemu przenieśliśmy embeddingi i model na sztudio a nie zostawiliśmy na silver, po co silverowi model?
Dobry wynik: decyzja "Silver nie powinien mieć modelu ani pełnego indexu"; sztudio = runtime truth z Ollama `qwen3-embedding:8b`, silver tylko klient/dostęp.
Zły wynik: sama komenda search/status bez "dlaczego sztudio"; albo wynik o innym hoście.
anchors_match: any_of
anchors:
- codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`Silver`, `sztudio`, `qwen3-embedding:8b`, `runtime truth`]
- claude__d7bbf152__2026-06-04__cab6a103 → expected_terms: [`qwen3-embedding:8b`, `4096`, `sztudio`]

### `aicx-ssh-openai-key`
type: evidence · scope: aicx
Pytanie: czemu semantic search pada przez ssh, cos z kluczem openai mimo ollamy?
Dobry wynik: config wymusza `api_key_env = "OPENAI_API_KEY"` mimo że Ollama nie potrzebuje klucza → twardy fail w `cloud.rs`; fix = zakomentować + ustawić `AICX_HOME`.
Zły wynik: generyczne "uruchom `aicx index`" / "indeks nie zbudowany" bez wskazania na klucz/config.
anchors: codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`OPENAI_API_KEY`, `cloud.rs`, `api_key_env`, `AICX_HOME`]

### `aicx-corpus-noise`
type: evidence · scope: aicx
Pytanie: ile z indexu to realnie ludzkie wiadomosci a ile maszynowy szum?
Dobry wynik: ~36% to `user_msg + agent_reply`, ~64% kanały maszynowe (system_note ~93k itd.) z 228018 rows.
Zły wynik: ogólnik "indeks duży" albo sama komenda statusu bez rozbicia per frame.
anchors: codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`228018`, `system_note`, `agent_reply`, `user_msg`]

### `aicx-oracle-dense-only`
type: evidence · scope: aicx
Pytanie: co znaczy ten `oracle_status ... semantic_dense_only ... hybrid_unavailable` i co z tym zrobilismy?
Dobry wynik: dense-only vector search bez warstwy hybrydowej/leksykalnej/rerankingu; zdrowy stan = `hybrid_rrf`; fix poprawił default (ukrycie tool_call/system_note, większy candidate pool).
Zły wynik: powtórzenie samego stringu statusu bez diagnozy/naprawy.
anchors: codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`hybrid_unavailable`, `semantic_dense_only`, `hybrid_rrf`]

### `aicx-derive-buckets`
type: evidence · scope: aicx
Pytanie: czy warto trzymac osobne project buckety, ile to zjada miejsca i czy cos dodaje?
Dobry wynik: project bucket to głównie latency cache (np. 3.24s vs ~68s), nie dodaje wiedzy; `derive --all-projects` rozdął `indexed/` do ~51G — odrzucone jako default.
Zły wynik: "buckety są lepsze" bez tradeoffu latency/storage; albo sama komenda derive.
anchors: codex__019ed8cb__2026-06-18__d0055557 → expected_terms: [`index derive`, `latency cache`, `51G`, `3.24s`]

### `aicx-intents-scan-vs-search`
type: evidence · scope: aicx
Pytanie: czy `aicx intents` searchuje po indeksie czy skanuje pliki, bo `--limit 3` i tak miele wszystko?
Dobry wynik: intents = timeline-reducer, nie search; `--hours` tnie na skanie, `--since/--until` na display, `--limit` jest display-only.
Zły wynik: opis `aicx search` zamiast `intents`, albo twierdzenie że limit ogranicza skan.
anchors: codex__019ed7c6__2026-06-17__ff7c50d1 → expected_terms: [`timeline`, `--limit`, `--hours`, `--since`]

### `aicx-since-exact-day`
type: evidence · scope: aicx
Pytanie: czemu `--since 2026-06-01` daje 0 wynikow a `--since 2026-06-01..` dziala?
Dobry wynik: bug — `parse_date_filter` dla pojedynczej daty zwraca `(Some(day), Some(day))`, czyli exact-day zamiast lower-bound jak mówi help.
Zły wynik: "to feature" albo workaround bez nazwania `parse_date_filter`.
anchors: codex__019ed7c6__2026-06-17__ff7c50d1 → expected_terms: [`parse_date_filter`, `--since`, `lower bound`]

---

## transcript-builder

### `tb-layers`
type: ask_answer · scope: transcript-builder
Pytanie: przypomnij co jest L0 / L1 / L2 w TB, ktory plik jest source of truth?
Dobry wynik: L0 = raw logs, L1 = `session_record.json` (forensic source of truth), L2 = `human.md` + `index_payload.jsonl`.
Zły wynik: pomieszanie warstw albo nazwanie spotlight/human jako "prawdy" zamiast session_record.
anchors: claude__0f082081__2026-06-05__8c77dec0 → expected_terms: [`L1`, `session_record.json`, `L2`, `index_payload`, `human.md`]

### `tb-r1-historical-snapshot`
type: ask_answer · scope: transcript-builder
Pytanie: czemu dorobilismy te labelki "historical snapshot, not runtime truth" — co poszlo nie tak wczesniej?
Dobry wynik: claim "P6 partial, refire needed" był prawdziwy tylko na zamknięciu sesji, live repo pokazał P6 done → ryzyko refire; stąd `snapshot_as_of`, `claim_scope=session_close`.
Zły wynik: ogólne "dla porządku" bez historii refire/staleness.
anchors: codex__019ec4b6__2026-06-14__3e56c850 → expected_terms: [`snapshot_as_of`, `claim_scope`, `session_close`, `P6`]

### `tb-r2-chunking`
type: evidence · scope: transcript-builder
Pytanie: czemu wielka sesja 960-turnowa dawala tylko 1 chunk i jak teraz chunkujemy?
Dobry wynik: 960-turn / 8MB → 1 chunk było "too flat"; rozbiliśmy na typed chunks (session_card, intent, decisions, deliverables, verification_gates, handoff, files_entities), klucz dedupe → `chunk_id`.
Zły wynik: sama definicja "round" z aicx albo nic o typed chunkach.
anchors: codex__019ec4c1__2026-06-14__231c0e5e → expected_terms: [`960`, `chunk_id`, `session_card`, `verification_gates`]

### `tb-human-vs-spotlight`
type: evidence · scope: transcript-builder
Pytanie: human.md mial byc one-page a robil sie 180 linii — jak go rozdzielilismy od spotlight?
Dobry wynik: `human.md` = one-page summary (~77 linii), `spotlight.md` = pełny clean-flow (np. 2583 linie) z `build-session-record`, bez wycinania środka, z czyszczeniem `toolu_`/`name: Bash`/`[tool result payload omitted]`.
Zły wynik: traktowanie human.md i spotlight.md jako to samo, albo "obetnij ogon" jako fix.
anchors_match: any_of
anchors:
- codex__019ec549__2026-06-14__a54f902c → expected_terms: [`human.md`, `spotlight.md`, `one-page`, `2583`]
- codex__019ec4c9__2026-06-14__fd6e76b5 → expected_terms: [`clean flow`, `toolu_`, `build-session-record`]

### `tb-index-payload-titles`
type: evidence · scope: transcript-builder
Pytanie: czemu w index_payload wszystkie chunki mialy te same tytuly i co to psulo?
Dobry wynik: long sessiony dawały N chunków z tym samym uciętym promptem; fix podniósł distinct titles (np. 7→33), dodał recall scoring i de-noising `files_entities`; też fix `verification_gates` z samym `jest: unknown`.
Zły wynik: tylko "tytuły brzydkie" bez związku z retrievalem/recall.
anchors: claude__761bc3a0__2026-06-14__a3c45682 → expected_terms: [`distinct titles`, `files_entities`, `recall_value`, `Memex`]

---

## md-radar

### `mdradar-intents-empty`
type: evidence · scope: aicx ∩ md-radar
Pytanie: czemu `aicx_intents` dla md-radar jest pusty — to lance, stale czy co?
Dobry wynik: ingest gap — intent-bogata 5h sesja ruszyła z `cwd=~/Git` (non-repo), nigdy nie trafiła do AICX; wpadł tylko szumowy ogon. Nie lance, nie stale.
Zły wynik: "przebuduj lance" / "to stale index" — czyli błędne tropy.
anchors: claude__cf9e5ef4__2026-06-15__2bd40f7b → expected_terms: [`md-radar`, `ingest`, `cwd`, `non-repo`]

### `mdradar-runtime-truth-status`
type: evidence · scope: md-radar
Pytanie: czy md-radar status mowi prawde po runtime-truth fix? worker# source# i ten head 314293b
Dobry wynik: runtime-truth fix rozdziela Launchd/Exec/Bin#/Worker#/Source#/Version; tu `Worker# == Source#`, `Version == HEAD (314293b)`, zero fałszywego WARN.
Zły wynik: trafienie w starą sesję sprzed fixa albo sam status bez wyjaśnienia co fix rozdzielił.
anchors: claude__5ac8b48a__2026-06-15__7ac8321b → expected_terms: [`Runtime-truth fix`, `Worker#`, `Source#`, `314293b`]

### `mdradar-readme-drift`
type: evidence · scope: md-radar
Pytanie: co z tym znikiem ↓ i martwym visit/last_visit_ts w md-radar readme?
Dobry wynik: README opisuje licznik `↓ od ostatniej wizyty` którego nie ma; `visit`/`last_visit_ts` to martwa funkcja (zapisywana, nieczytana); żywy nagłówek dasha ma 3 liczniki (`725 nowe` itd.).
Zły wynik: opis aktualnego dasha bez wskazania drftu README↔kod / martwej funkcji.
anchors: claude__5ac8b48a__2026-06-15__7ac8321b → expected_terms: [`last_visit_ts`, `visit`, `README`, `725 nowe`]

---

## Pensieve

### `pensieve-autocomplete-mock`
type: evidence · scope: pensieve (vista-kernel FFI)
Pytanie: czemu AI autocomplete chodzi w produkcji na mocku, ten cały VistaEngine wydmuszka?
Dobry wynik: realny `VistaEngine.complete()` jest w FFI (`qube_ffi.swift.patch:629`) ale nic go nie podpina → default `MockVistaAutocompleteEngine` zwraca literalne " continuation"; mock żyje w prod i ma `fatalError` (loadConfig, transcribeFile) = crash.
Zły wynik: "autocomplete działa" / czysto frontendowa odpowiedź bez FFI/mock.
anchors: claude__f30e5b56__2026-06-10__ec142ff3 → expected_terms: [`MockVistaAutocompleteEngine`, `qube_ffi.swift.patch:629`, `continuation`, `fatalError`]

### `pensieve-stoprecording-cadence`
type: evidence · scope: pensieve
Pytanie: ten bug gdzie po wyjatku w stopRecording dalej leci nagrywanie i cadence — co bylo nie tak?
Dobry wynik: `stopRecording()` w error-path zostawia `isRecording=true` i żywą pętlę cadence — throw omija `isRecording=false` i `stopCadenceCommitLoop()`; zalecany `defer`.
Zły wynik: ogólne "obsłuż wyjątek" bez wskazania `isRecording`/cadence loop w TranscriptionService.
anchors: claude__f30e5b56__2026-06-10__ec142ff3 → expected_terms: [`stopRecording`, `isRecording`, `stopCadenceCommitLoop`, `TranscriptionService.swift.patch`]

### `pensieve-pr7-ci-gate`
type: ask_answer · scope: pensieve
Pytanie: czemu na pr #7 nie ma zadnych checków na githubie i co jest gate'em zamiast ci?
Dobry wynik: CI (`ci.yml`) odpala się tylko na `pull_request targeting main`; PR #7 celuje w inny branch → brak CI by config; gate to lokalny `make test` (336 green).
Zły wynik: "CI jest zepsute" zamiast wyjaśnienia że trigger nie łapie tego brancha + lokalny gate.
anchors: claude__f30e5b56__2026-06-11__a512f197 → expected_terms: [`ci.yml`, `pull_request targeting`, `make test`, `336`]

---

## ScreenScribe

### `screenscribe-race-disaster`
type: evidence · scope: ScreenScribe
Pytanie: czemu nie odpalamy dwoch sesji na screenscribe-seed naraz, byl jakis race?
Dobry wynik: "race disaster" — dwie sesje na `screenscribe-seed` → living-tree race; decyzja: fan-out tylko do myślenia, write single-session inline, bo równoległe blind-writes = powrót awarii.
Zły wynik: ogólne "git konflikt" bez living-tree race / decyzji single-session.
anchors: claude__3d2716aa__2026-06-14__7fbabcf5 → expected_terms: [`race`, `living-tree`, `single-session`, `screenscribe-seed`]

### `screenscribe-aicx-home-mismatch`
type: evidence · scope: ScreenScribe
Pytanie: czemu semantic search w screenscribe walil index_not_built wszedzie, cos z home path?
Dobry wynik: rescan pisał do `~/aicx/indexed/_all/...` (bez kropki), search czytał z `~/.aicx/indexed/_all/...` (z kropką) → indeks budowany w jednym home, szukany w drugim.
Zły wynik: "przebuduj indeks" bez wskazania na rozjazd `aicx` vs `.aicx` home.
anchors: claude__3d2716aa__2026-06-14__7fbabcf5 → expected_terms: [`index_not_built`, `.aicx/indexed`, `~/aicx`, `_all`]

### `screenscribe-confirmed-to-verdict`
type: evidence · scope: ScreenScribe
Pytanie: ten rename confirmed -> verdict, czemu zabilismy dual-model?
Dobry wynik: nie chcemy dual-modelu `confirmed`(bool)/`verdict`; po cutcie jeden język = `human_review.verdict` (`accepted|rejected|none`).
Zły wynik: trafienie w niezwiązany "verdict" z innego repo/sesji (TB verdict enum) zamiast screenscribe rename.
anchors: claude__3d2716aa__2026-06-14__7fbabcf5 → expected_terms: [`confirmed`, `verdict`, `human_review`, `accepted`]

### `screenscribe-transcribe-lang-default`
type: ask_answer · scope: screenscribe
Pytanie: mowilam po polsku w video a screenscribe przetlumaczyl na angielski mimo ze w configu mam language pl — czemu?
Dobry wynik: CLI ma default `--lang en`, więc bez flagi STT dostaje `language=en` i ignoruje `SCREENSCRIBE_LANGUAGE=pl` z config.env → "polski sens po angielsku".
Zły wynik: "model słabo radzi sobie z polskim" bez wskazania na default `--lang en` nadpisujący config.
anchors: codex__019eb27b__2026-06-10__0d101f81 → expected_terms: [`SCREENSCRIBE_LANGUAGE`, `--lang en`, `transcribe`, `language=en`]

### `screenscribe-manualframe-css-leak`
type: evidence · scope: screenscribe
Pytanie: w review html pro tekst manual frame karty laduje sie pod miniaturka — co to za bug z css?
Dobry wynik: globalna reguła `.annotation-container { display: inline-block }` z `report-pro.css` przecieka do manual card (miniatura w `.annotation-container`), więc tekst ląduje za thumbnailem.
Zły wynik: "popraw z-index" bez wskazania na leak `.annotation-container` z report-pro.css.
anchors: codex__019ec0c7__2026-06-13__89e1cf3c → expected_terms: [`annotation-container`, `report-pro.css`, `manual-frame`, `inline-block`]

### `screenscribe-save-reload-localstorage`
type: ask_answer · scope: screenscribe
Pytanie: screenscribe gubi moje decyzje review po reloadzie w innej przegladarce — gdzie sie zapisuje human_review?
Dobry wynik: `/api/save` zapisuje `human_review` do `report.json` na dysku, ale klient przy starcie czyta draft z `localStorage` → decyzje giną na świeżym loadzie; fix = WorkItem + read-state endpoint + hydracja z dysku.
Zły wynik: "wyczyść cache" bez wskazania rozjazdu `localStorage` vs `report.json`.
anchors: codex__019ec108__2026-06-13__ec2c5ae5 → expected_terms: [`human_review`, `localStorage`, `report.json`, `/api/save`]

---

## Loctree

### `loctree-mcp-spawn-fail`
type: evidence · scope: loctree-mcp / Claude Desktop
Pytanie: claude desktop sypie sie na loctree mcp, "failed to spawn process" — co to bylo i jak naprawione?
Dobry wynik: config wskazywał `command` na `~/.cargo/bin/loctree-mcp` którego nie ma; żywa binarka (0.13.0-dev) jest w `~/.local/bin/`; fix = jedna linia w `claude_desktop_config.json` + pełny restart.
Zły wynik: "przeinstaluj loctree" bez wskazania złej ścieżki w configu.
anchors: claude__e20908f4__2026-06-17__31add79e → expected_terms: [`Failed to spawn process`, `.cargo/bin`, `.local/bin`, `claude_desktop_config.json`]

### `loctree-context-pill-dom-id`
type: evidence · scope: loctree-suite (VS Code Context Pill)
Pytanie: czemu Context Pill webview sie nie renderuje, cos z DOM id mismatch?
Dobry wynik: cross-file bug — Context Pill webview nie renderuje przez niezgodność DOM id między HTML a skryptem; trafienie w tę sesję/fix.
Zły wynik: ogólny "webview nie działa" bez DOM id mismatch, albo inny loctree audit.
anchors: claude__f3146748__2026-06-06__d26b8a53 → expected_terms: [`Context Pill`, `DOM id`, `webview`]

### `loctree-literal-cache-stale`
type: ask_answer · scope: loctree-suite (LSP cache)
Pytanie: czemu loctree lsp literal cache zwraca stale wyniki po zapisaniu pliku bez commita?
Dobry wynik: klucz cache (`git_scan_id` = branch@short-HEAD) jest na granularności committed-HEAD, a `scan_literal` czyta żywy dysk → edycja bez commita (dominujący workflow) nie zmienia klucza → stale.
Zły wynik: "zrestartuj LSP" bez wskazania że cache key jest na committed HEAD a skan czyta `uncommitted` dysk.
anchors: claude__f3146748__2026-06-06__3cef4590 → expected_terms: [`LiteralScanCache`, `git_scan_id`, `scan_literal`, `uncommitted`]

### `loctree-slownet-hero-fallback`
type: evidence · scope: loctree-suite (loctree.com landing)
Pytanie: loctree landing slow net fallback — po ile ms pokazuje sie static headline i kiedy odpalaja sie reveal jak wasm nie dojedzie?
Dobry wynik: static headline po `grace 600ms`, `.reveal` `fail-open` po 1.4s gdy WASM nie wstanie; podwójny headline to celowy a11y (`sr-only`), nie bug.
Zły wynik: "to bug z podwójnym nagłówkiem" zamiast celowego sr-only fallbacku + timingów.
anchors: claude__65e6ccc9__2026-06-10__2ec2f3e6 → expected_terms: [`fail-open`, `grace 600ms`, `sr-only`, `reveal`]

### `loctree-0124-checksums-signing`
type: ask_answer · scope: loctree (release 0.12.4)
Pytanie: dlaczego darwin bundle 0.12.3 mial 6/6 failed checksums po rozpakowaniu i jaka jest poprawna kolejnosc podpisywania?
Dobry wynik: żywy darwin 0.12.3 ma `6/6 FAILED` internal CHECKSUMS (liczone przed codesign) i nic w pipeline tego nie gate'uje; poprawna kolejność: `codesign → notarize → repack → checksums`.
Zły wynik: "przelicz checksumy" bez wyjaśnienia że muszą być PO podpisie.
anchors_match: any_of
anchors:
- claude__20ff45fb__2026-06-11__0b3410d2 → expected_terms: [`6/6 FAILED`, `CHECKSUMS`, `codesign`]
- claude__20ff45fb__2026-06-12__5635a8ff → expected_terms: [`codesign`, `notarize`, `repack`, `CHECKSUMS`]

---

## Agent-blackbox

### `blackbox-i18n-verdict-tokens`
type: evidence · scope: agent-blackbox (i18n B7)
Pytanie: w blackboxie ten i18n — czemu linia `powody:` zostaje po angielsku mimo PL?
Dobry wynik: świadoma decyzja — tokeny verdiktów, składnia komend i schema JSON zostają po angielsku; tłumaczona jest proza/etykiety, a powody to człowiecza proza, nie stabilny token jak `potential-treasure`.
Zły wynik: "to bug, przetłumacz powody" — odwrotnie do decyzji.
anchors: claude__0009bbad__2026-06-05__a128708f → expected_terms: [`powody:`, `i18n`, `potential-treasure`]

---

## vibecrafted (tooling silver/sztudio)

### `vibecrafted-path-doctor-authority`
type: evidence · scope: vibecrafted / tooling
Pytanie: czemu path-doctor krzyczy FAIL na vc-* a `vibecrafted doctor` mowi ok? cos z venv/bin entrypointami
Dobry wynik: path-doctor ma stary/ostrzejszy kontrakt `PYTHON_ENTRYPOINT_LAUNCHERS` (oczekuje symlinka do `vibecrafted`), a runtime packaging poszedł na `.venv/bin` → fałszywe FAIL (failures=16); repo doctor traktuje to jako OK.
Zły wynik: "napraw symlinki vc-*" bez root-cause stale-contract vs `.venv/bin`.
anchors: codex__019ebea6__2026-06-13__9e44fe9f → expected_terms: [`path-doctor`, `PYTHON_ENTRYPOINT_LAUNCHERS`, `.venv/bin`, `failures=16`]

### `vibecrafted-doctor-verbose-flag`
type: evidence · scope: vibecrafted / tooling
Pytanie: `vibecrafted doctor --verbose` nie pokazuje verbose, czemu? cos z `$@` w wrapperze?
Dobry wynik: wrapper woła `python3 "$installer" doctor` bez `"$@"`, więc `--verbose` ginie — `scripts/vibecrafted:1861`.
Zły wynik: "doctor nie ma trybu verbose" zamiast wskazania na zgubiony `"$@"` w wrapperze.
anchors: codex__019ebea6__2026-06-13__9e44fe9f → expected_terms: [`--verbose`, `scripts/vibecrafted:1861`, `"$@"`]

---

## sekrety (zredagowane)

### `secrets-plaintext-rotate`
type: evidence · scope: transcript-builder / higiena (REDACTED)
Pytanie: gdzie mialam ten klucz plaintextem i co trzeba z nim zrobic?
Dobry wynik: `OPENAI_API_KEY` zapisany plaintextem w `~/.zshrc` (ok. linii 43) → potraktować jako spalony, **zrotować** i przenieść do osobnego `secrets.zsh`. (Bez pokazywania wartości klucza.)
Zły wynik: zacytowanie/wyświetlenie samego klucza, albo zignorowanie rekomendacji rotacji.
anchors: claude__21852ec7__2026-06-14__ae23ef0f → expected_terms: [`OPENAI_API_KEY`, `.zshrc`, `plaintext`, `zrotuj`]
> Polityka: w pytaniu i `expected_terms` żadnych fragmentów kluczy — tylko nazwa
> zmiennej, lokalizacja i akcja (rotacja). To samo dotyczy innych wątków
> sekretów (np. plaintext `BRAVE_API_KEY`/GitHub token w `claude_desktop_config.json`).

---

## Notatki do harnessu

- To seed dla `aicx eval search-quality` (human-facing evidence drift), osobny od
  backendowego 50-query gold setu w `queries.toml`.
- `expected_terms` to kontrakt **treściowy** sprawdzany w body `.md` — celowo, bo
  trafienie po `map_id` z `.meta.json` potrafi być puste/słabe (patrz wycięte dawne
  #16). Term obecny w jednej gęstej linii meta/summary ≠ retrievalny chunk.
- Dla `type = ask_answer` mierzymy czy wskazana sesja/chunk jest trafna, nie czy
  narracja jest ładna.
- Przy migracji do TOML harnessu: `expected_terms` → kontrola treści, a `map_id` →
  mapowanie na kanoniczne ścieżki `~/.aicx/store/...` danej sesji.
