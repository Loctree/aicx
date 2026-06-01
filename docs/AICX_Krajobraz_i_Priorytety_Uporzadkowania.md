# Mapa Krajobrazu AICX – Stan na dziś + Obszary do Uporządkowania

**Data:** 2026-05-28  
**Branch:** `claude/aicx-uniformity`  
**Commit:** `f0ad934` (z aktualnym snapshotem loctree)

Poniższy dokument łączy trzy perspektywy:
1. Krótka, ostra diagnoza
2. Głębsza analiza „What I See and Do Not Understand”
3. Konkretna mapa dekompozycji i porządkowania z priorytetami

---

## 1. Aktualny Krajobraz Architektoniczny

**Warstwy systemu (od dołu do góry):**

| Warstwa                        | Stan                        | Rozmiar / Charakter              | Komentarz |
|--------------------------------|-----------------------------|----------------------------------|---------|
| **Core Parser** (`crates/aicx-parser`) | Najzdrowsza część systemu | ~6,8k LOC, 16 plików | Czysta, dobrze odseparowana, niska złożoność. Najcenniejszy asset. |
| **Store** (`src/store.rs` + podmoduły) | Największy hub | 2195 LOC + ~2k w podmodułach | Najwyższa liczba importerów w systemie. |
| **Sources**                    | W fazie przejściowej | `legacy.rs` 6111 LOC + struktura | Największy aktualny ból. |
| **Main + Orkiestracja**        | Duży i płaski | `src/main.rs` ~7100 LOC | Mieszanka CLI + logiki biznesowej. |
| **Retrieval & Intelligence**   | Średnio-sprzężone | 1,3k–2,5k LOC każdy | Silnie zależne od Store + Sources. |
| **Powierzchnie** (MCP, Dashboard, Wizard, Doctor) | Rosnące | Różne | Często obchodzą lub duplikują logikę. |

**Najważniejsze obserwacje strukturalne:**
- Bardzo wyraźna, zdrowa warstwa parsera.
- Dwa największe źródła złożoności: `src/store.rs` i `src/sources/legacy.rs`.
- Dużo re-eksportów (szczególnie ze `store`).
- 37 duplicate exports (twins) – głównie skutek uboczny dużych plików.

---

## 2. Aspiracja vs Rzeczywistość

**Aspiracja (to, co kod i nazewnictwo sugerują):**

- Zaawansowany **Universal Operator Memory Engine**.
- Silna, stabilna kanoniczna reprezentacja (`TimelineEntry`).
- Czysty podział: źródła → normalizacja → pamięć kanoniczna → powierzchnie.
- Łatwość dodawania nowych źródeł (Jira, Notion, Slack, itp.).
- Wysoka jakość i odtwarzalność danych.

**Rzeczywistość:**

- Hybryda ambitnej architektury z organicznym wzrostem.
- Najważniejsza warstwa (ingest + canonical store) jest najmniej modularna.
- `TimelineEntry` jest już w dużej mierze w formie docelowej, ale infrastruktura wokół niego nie nadąża.
- Dużo logiki „wie”, dokąd system zmierza, ale implementacja jest jeszcze w dużym stopniu historyczna.

---

## 3. Delta – Największe Przepaści

| Obszar                  | Aspiracja                              | Rzeczywistość                          | Rozmiar luki     | Priorytet |
|-------------------------|----------------------------------------|----------------------------------------|------------------|---------|
| **Ingest (Sources)**    | Czyste, odseparowane adaptery          | Duży monolit + częściowa dekompozycja  | Bardzo duża      | **1** |
| **Store**               | Dobrze wydzielona warstwa              | Największy hub w systemie              | Duża             | **2** |
| **Main / CLI**          | Cienka warstwa dispatch                  | Duży plik z mieszanką definicji i logiki | Średnia-duża   | **3** |
| **Entity & Session Graph** | Świadomość encji i tożsamości w czasie | Rozproszona, niekompletna              | Średnia          | Wysoki |
| **Universal Sources**   | Łatwe dodawanie Notion, Jira, Slack    | Prawie niemożliwe bez dotykania legacy | Bardzo duża      | Długi termin |
| **Jakość modułów**      | Spójna jakość                          | Parser wyraźnie lepszy niż reszta       | Wysoka           | Średni |

---

## 4. Mapa Krajobrazu do Uporządkowania (proponowana kolejność)

**Fala 1 – Fundament Ingestu (najwyższy zwrot)**
- Dokończyć dekompozycję `src/sources/` (shared + providers)
- Cel: `src/sources/legacy.rs` poniżej 2500–3000 LOC

**Fala 2 – Store jako warstwa**
- Wydzielić migracje, context corpus logic, query helpers
- Zmniejszyć liczbę re-eksportów

**Fala 3 – Main i CLI**
- Wyodrębnić powierzchnię CLI
- Przenieść ciężką logikę komend

**Fala 4 – Wzmocnienie Graphów (A + B)**
- Praca nad `entity_canonical_id` i tożsamością sesji

**Fala 5 – Przygotowanie pod Universal Sources (F)**
- Projekt interfejsu adapterów źródłowych

---

## 5. Rekomendacja na najbliższe 2–4 tygodnie

1. Kontynuować agresywnie dekompozycję `sources` (najczystszy i najbardziej opłacalny ruch).
2. Po każdej większej fali robić krótkie `loct slice` + `loct find`, żeby widzieć spadek couplingu.
3. Zrównoleglić małą, ale konkretną akcję w `store.rs` (np. wydzielenie migracji).
4. Nie ruszać jeszcze dużymi falami `main.rs`, dopóki `sources/legacy.rs` nie zejdzie wyraźnie poniżej 4000 LOC.

---

**Status:** Dokument roboczy. Będzie aktualizowany w miarę postępu dekompozycji.
