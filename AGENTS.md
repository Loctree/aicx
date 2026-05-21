<!-- loctree-doctrine: v1 -->
## **LOCTREE + AICX + VIBECRAFTED — ZŁOTE RUNO**

> **Loctree first, brak doubt. Grep = potwierdzony hak.**

Strukturalna percepcja PRZED każdym sięgnięciem po `grep`/`awk`/`sed`/
`find`/`Read+offset`. Plus aicx jako historia intencji, vibecrafted jako
dyscyplina dowodu. Trio jest kanonem.

**Reguła operacyjna:**

- Pierwszy ruch przy każdym strukturalnym pytaniu (kto importuje X,
  gdzie żyje symbol Y, co pęknie po edycji Z, blast radius, struktura
  katalogu A) → `loctree-mcp` tool (`context` / `slice` / `impact` /
  `find` / `focus` / `follow`).
- Każde sięgnięcie po `grep`/`awk`/`sed`/`find` na rzeczy która
  **powinna być** loctree-side = **hak**. Zapisz wpis do backlogu
  (`cuts/loctree-haki.md` per-repo albo operator-managed global).
- "Doubt" w wyborze tool = anti-pattern. Albo loctree to znajdzie,
  albo nie umie i wtedy hak + fallback.
- Sfabrykowane doctriny ("CodeScribe grep-first", "szybciej grepem",
  "loctree pewnie nie ma") = halucynacja klasy `cutoffflu`. Zakaz.
- `loctree-mcp` niedostępne? Użyj `loct` cli, ale napisz 'haka'
   sygnalizującego ten problem.

**Lokalizacja backloga "Loctree fail":**

- Pisz **na końcu** pliku ~/.vibecrafted/loctree/loctree-fail.md
- Nie twórz na nowo, nie nadpisuj - to plik przeznaczony do appendowania. 
- Nie musisz czytać istniejących wpisów. Jeśli Twój hak jest zgłoszony
  kolejny raz to sygnał o jego trafności, a nie powielanie.

**Dlaczego:** Vista (duet weterynarzy × AI agents) to istniejący proof.
Loctree perfection skaluje ten model do każdego foundera nieprogramisty
bez milionów. Continuous backlog closure = warunek wiarygodności tej tezy.

<!-- /loctree-doctrine -->

# aicx — Repo Guidelines

<!-- Per-repo, agent-agnostic instructions. Edit below this line. -->
<!-- The doctrine block above is operator-managed via                -->
<!-- ~/.claude/scripts/loctree_doctrine_scan.sh and should not be    -->
<!-- edited inline — use --revert + re-apply to refresh it.          -->

_Seeded 2026-05-14 by `loctree_doctrine_scan.sh --seed`._

## Bug-fix workflow

Przed otworzeniem nowego fixa:

1. `docs/BACKLOG.md` — sprawdź czy bug jest już znany / w trakcie / partial.
   Append-only, najnowsze na dole. Status na końcu wpisu po `—`:
   `open` / `investigating` / `done(@sha)` / `partial(@sha — co; co zostaje)`.
2. `docs/BUGFIXES.md` — pamiętnik fix-historii (symptom → root cause → fix →
   tests → lessons). Najnowsze na dole. Czytaj lessons z poprzednich wpisów —
   bug-patterns często wracają w innej skórze (identity inference z treści,
   substring match na user input, `as` cast na user-provided integer, plik
   istniejący ≠ kompilowany w bin crate, …).
3. Loctree-first dla strukturalnych pytań (patrz doctrine block wyżej).

Po zamknięciu fixa:

1. Dodaj wpis do `docs/BUGFIXES.md` używając protokołu z headera tego pliku
   (PL prose, EN identyfikatory, append-only).
2. Jeśli fix zamyka pozycję z `BACKLOG.md` — zaktualizuj jej status tag.
   Pełne zamknięcie: `done(@sha)`. Częściowe: `partial(@sha — co zrobione;
   co zostaje open)`. Nie usuwaj wpisu — protokół BACKLOG jest append-only.
3. Większe incydenty (data loss, segfault, security, masowy quarantine) →
   osobny plik `docs/incidents/<YYYY-MM-DD>_<slug>.md` z forensiką, plus
   ref w wpisie BUGFIXES (sekcja `Related.`).
4. User-facing release notes idą do `CHANGELOG.md` (Keep a Changelog),
   nie do BUGFIXES — to dwie różne publiki.
