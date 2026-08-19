# Vetcoders HTTP MCP Services Runbook

**Wersja:** 1.0.0 (Sierpień 2026)
**Status:** Kanoniczna Architektura Multi-Agent & Multi-Machine
**Zakres:** `aicx-mcp` (Pamięć & Wyszukiwanie Intencji) + `loctree-mcp` / `loct watch --http` (Strukturalna Percepcja Kodu)

---

## 1. Dlaczego Streamable HTTP zamiast Stdio?

W środowiskach wieloagentowych (flota 10–50 agentów Codex, Claude, Gemini, Antigravity, subagenci):

| Cecha | Tryb Stdio | Tryb Streamable HTTP (Zalecany) |
| :--- | :--- | :--- |
| **Liczba procesów** | 1 proces per agent (50 agentów = 50 procesów w `ps aux`) | **1 centralny demon na maszynie** |
| **Zużycie RAM** | Duplikacja pamięci per proces (50 × 50MB+) | **Współdzielona pamięć RAM (~30–60MB na proces)** |
| **Zarządzanie cache** | Wyścigi dyskowe o pliki `.cache` | **Jedno in-memory `Arc<Snapshot>` w pamięci RAM** |
| **Szybkość zapytań** | Start binarki + cold read per wywołanie | **< 1–5 ms (natychmiastowy odczyt grafu z RAM)** |
| **Dostęp zdalny** | Tylko lokalnie na maszynie roboczej | **Dostęp po sieci / Tailscale (`dragon`, `div0`, `sztudio`)** |
| **Skalowalność** | Dławi CPU i wyczerpuje limity deskryptorów plików | **Tokio + Axum bez problemu obsługuje tysiące zapytań** |

---

## 2. Serwis 1: `aicx-mcp` (Silnik Pamięci i Wyszukiwania Intencji)

Instalowana usługa domyślnie nasłuchuje wyłącznie na `127.0.0.1`. Wystawienie
jej do Tailnetu jest jawną decyzją operatora:

```bash
AICX_MCP_HOST="$(tailscale ip -4)" \
  AICX_MCP_ALLOWED_HOSTS="$(hostname -s),$(tailscale ip -4),localhost,127.0.0.1" \
  make install-service
```

Adres klienta pozostaw na loopbacku, chyba że klient faktycznie działa na innej
zaufanej maszynie Tailnetu. Nie commituj wartości wygenerowanego tokena — użyj
pliku tokena albo mechanizmu zmiennej/nagłówka danego klienta.

### 2.1. Odświeżanie na Żywo (Live Refresh)
* **Czy `aicx serve` odświeża się na żywo?** **Tak.** `aicx serve` nie zamraża stanu indeksu w RAM na stałe. Każde zapytanie narzędzia (`aicx_search`, `aicx_steer`, `aicx_intents`) odpytuje bezpośrednio bieżący stan bazy Tantivy oraz wektorów z dysku (`~/.aicx/`).
* **Cykl indeksowania:** Tryb HTTP ma własną ograniczoną pętlę async: co 5 minut odświeża gorące 48 godzin katalogu i inkrementalnie publikuje indeks leksykalny. Blokująca praca plikowa i indeksowanie wykonują się poza workerami requestów Tokio. Interwał zmienisz przez `--refresh-interval-seconds`; `--no-auto-refresh` ma sens tylko wtedy, gdy świeżość ma innego jawnego właściciela.

### 2.2. Instalacja i Cele w Makefile
* **Standardowa instalacja (z kreatorem):**
  ```bash
  cd /Volumes/vc-workspace/Loctree/aicx
  make install
  ```
  *(Uruchamia `install.sh`, instaluje binarki, rejestruje `io.vetcoders.aicx.mcp` i zapisuje klientom Claude/Codex/Gemini `mcpServers.aicx` jako `{"url":"http://127.0.0.1:8044/mcp"}` — nie stdio `command`. Ten serwer jest właścicielem odświeżania indeksu. `AICX_SKIP_MCP_SERVICE=1` zostawia stdio.)*

* **Zarządzanie serwisem LaunchAgent:**
  ```bash
  make install-service    # Instaluje i startuje LaunchAgent io.vetcoders.aicx.mcp
  make uninstall-service  # Zatrzymuje i wyrejestrowuje LaunchAgent io.vetcoders.aicx.mcp
  make install-schedule   # Legacy: osobny refresh tylko gdy nie działa serwer HTTP
  ```

### 2.3. Weryfikacja Działania (Smoke Test)
```bash
# 1. Healthcheck
curl -s http://127.0.0.1:8044/health && echo " (Health: OK)"

# 2. Handshake MCP
curl -s \
  -H "Authorization: Bearer $(cat ~/.aicx/auth-token)" \
  -H "Accept: application/json, text/event-stream" \
  -H "Content-Type: application/json" \
  -X POST http://127.0.0.1:8044/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke-test","version":"1.0"}}}'
```

---

## 3. Serwis 2: `loctree-mcp` (Uniwersalna Percepcja Kodu)

### 3.1. Dwa Modele Działania

#### Tryb A: Uniwersalny Serwer Centralny (Współdzielony dla wszystkich repozytoriów)
Jeden serwer na porcie `5174` obsługuje dowolne repozytorium. Każde wywołanie narzędzia (`slice`, `impact`, `find`, `focus`, `repo-view`, `tree`) przyjmuje parametr `project: "/sciezka/do/repo"`.
* **Pojemność:** Działa z flagą `--snapshot-cache-capacity 20`, trzymając w RAM snapshoty dla nawet 20 repozytoriów jednocześnie.

#### Tryb B: Dedykowany Watcher per-repo (`loct watch --http`)
W katalogu aktywnie rozwijanego projektu:
```bash
loct watch --http --port 5174 &
```
* Obserwuje zmiany w plikach i natychmiast przelicza snapshot po zapisie.
* Samorządnie nadzoruje proces potomny `loctree-mcp` na `127.0.0.1:5174/mcp`.

### 3.2. Instalacja i Cele w Makefile
* **Standardowa instalacja w `loctree-suite`:**
  ```bash
  cd /Volumes/vc-workspace/Loctree/loctree-suite
  make install-all
  ```
  *(Kompiluje `loct`, `loctree`, `loctree-mcp`, `loctree-lsp`, podpisuje binarki i rejestruje serwis launchd `io.vetcoders.loctree.mcp`).*

* **Zarządzanie serwisem LaunchAgent:**
  ```bash
  make install-service    # Instaluje i startuje LaunchAgent io.vetcoders.loctree.mcp
  make uninstall-service  # Zatrzymuje i wyrejestrowuje LaunchAgent io.vetcoders.loctree.mcp
  ```

### 3.3. Weryfikacja Działania (Smoke Test)
```bash
curl -s \
  -H "Host: 127.0.0.1:5174" \
  -H "Accept: application/json, text/event-stream" \
  -H "Content-Type: application/json" \
  -X POST http://127.0.0.1:5174/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke-test","version":"1.0"}}}'
```

---

## 4. Matryca Konfiguracji Klientów MCP

### 4.1. Antigravity / Gemini IDE (`~/.gemini/config/mcp_config.json`)

```json
{
  "mcpServers": {
    "aicx-http": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8044/mcp",
      "headers": {
        "Authorization": "Bearer <TOKEN_Z_~/.aicx/auth-token>"
      }
    },
    "loctree-http": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:5174/mcp"
    }
  }
}
```

### 4.2. Claude Code (`~/.claude/mcp.json` lub projektowe `.mcp.json`)

```json
{
  "mcpServers": {
    "aicx": {
      "url": "http://127.0.0.1:8044/mcp",
      "headers": {
        "Authorization": "Bearer <TOKEN_Z_~/.aicx/auth-token>"
      }
    },
    "loctree": {
      "url": "http://127.0.0.1:5174/mcp"
    }
  }
}
```

Lub przez CLI:
```bash
claude mcp add --scope user --transport http \
  --header "Authorization: Bearer $(cat ~/.aicx/auth-token)" \
  aicx http://127.0.0.1:8044/mcp

claude mcp add --scope user --transport http \
  loctree http://127.0.0.1:5174/mcp
```

### 4.3. Codex (`~/.codex/config.toml`)

```toml
[mcp_servers.aicx]
url = "http://127.0.0.1:8044/mcp"
bearer_token_env_var = "AICX_MCP_TOKEN"

[mcp_servers.aicx.tools.aicx_search]
approval_mode = "approve"

[mcp_servers.aicx.tools.aicx_steer]
approval_mode = "approve"

[mcp_servers.aicx.tools.aicx_rank]
approval_mode = "approve"

[mcp_servers.loctree]
url = "http://127.0.0.1:5174/mcp"
```

---

## 5. Serwisy macOS launchd LaunchAgents

Serwisy są zarejestrowane jako per-user LaunchAgents w `~/Library/LaunchAgents/` z parametrami `RunAtLoad=true` i `KeepAlive=true`:

| Identyfikator Serwisu | Polecenie | Domyślny Port | Cel Logowania |
| :--- | :--- | :--- | :--- |
| **`io.vetcoders.aicx.mcp`** | `aicx serve --transport http` | `8044` | `~/.aicx/logs/aicx-serve-http.log` |
| **`io.vetcoders.loctree.mcp`** | `loctree-mcp --transport http` | `5174` | `~/.loctree/logs/loctree-serve-http.log` |

Poprzedni timer `io.vetcoders.aicx.reindex` jest usuwany przy instalacji serwera HTTP, aby nie utrzymywać dwóch konkurujących writerów indeksu.

---

## 6. Przydatne Aliasy i Skrypty Operacyjne

Dodaj poniższe aliasy do swojego `~/.zshrc`:

```bash
# Sprawdzenie statusu nasłuchujących portów MCP
alias mcp-status='lsof -nP -iTCP:8044,5174 -sTCP:LISTEN'

# Podgląd logów obu serwerów na żywo
alias mcp-logs='tail -f ~/.aicx/logs/aicx-serve-http.log ~/.loctree/logs/loctree-serve-http.log'

# Sprawdzenie stanu serwisów w launchd
alias mcp-launchd='launchctl list | grep vetcoders'

# Restart obu serwisów launchd
alias mcp-restart='launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.aicx.mcp.plist 2>/dev/null; \
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.aicx.mcp.plist; \
  launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.loctree.mcp.plist 2>/dev/null; \
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.loctree.mcp.plist; \
  echo "🚀 Serwisy zrestartowane w launchd"'
```
