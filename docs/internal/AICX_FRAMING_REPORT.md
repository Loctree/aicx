# AICX Framing Report: Perception Over Memory

## Overview
This report addresses the need to reframe the public promise of `aicx` (AI Contexters) from a heavy "memory layer" or "vector memory" paradigm to a more realistic and useful "perception, retrieval, and selective re-entry" paradigm. As AI agents increasingly rely on their own context window (perception), tooling should act as an organized shelf map (card catalog) rather than a magical backpack that automatically injects entire libraries of past thought.

This shift aligns with the core VetCoders AI Philosophy: "Perception over Memory: AI needs eyes, not a heavy backpack of history."

## 1. Identified Overclaims and Misleading Language

Based on a review of the current repository surfaces (`README.md`, `docs/`, `src/mcp.rs`, `src/main.rs`, `install.sh`, etc.), several phrases contribute to a misleading "memory layer" promise:

*   **"Memory extraction" & "vector memory":**
    *   `README.md`: "Memory extraction + context distillation..." and "optional sync into memex (vector memory)."
    *   `src/memex.rs`: "Integration with rmcp-memex for vector memory indexing."
    *   `src/main.rs`: "Sync stored chunks to rmcp-memex vector memory."
    *   *Why it's misleading:* "Memory" implies the agent will passively *remember* things. `aicx` does not give the agent memory; it gives the agent an index it can actively search and read. Calling memex "vector memory" reinforces the "backpack" fallacy. It's a semantic search index, not a cognitive memory layer.
*   **"Session history" (when used as a passive noun):**
    *   `install.sh`: "aicx_search — fuzzy search across session history"
    *   `docs/vetcoders-suite-showcase.html`: "Extracts prior session history..."
    *   `src/mcp.rs`: "...any AI agent can query session history..."
    *   *Why it's misleading:* While accurate, it frames the data as an amorphous blob of history rather than discrete, actionable records. It should be framed as a "timeline index" or "context ledger."

## 2. Proposed Public Doctrine: Perception Over Memory

We propose shifting the language across all user-facing surfaces to reflect the following doctrine:

**AICX is a Timeline Index, not a Memory Backpack.**
AI agents work best when they can clearly *perceive* their environment. `aicx` takes the messy exhaust of past agent sessions and turns it into a highly organized, ranked card catalog. It does not force the agent to memorize the past; it empowers the agent to quickly lookup, retrieve, and selectively re-enter previous work contexts.

**Key Metaphors to Adopt:**
*   **Instead of "Memory Layer", use "Context Ledger" or "Timeline Index".**
*   **Instead of "Session History", use "Extracted Timelines" or "Agent Records".**
*   **Instead of "Vector Memory", use "Semantic Search Add-on" or "Vector Index".**

## 3. Boundary Separation: Core, Memex, and Agent Perception

To set correct user expectations, the boundaries between the tools must be explicit:

### A. AICX Core (The Card Catalog)
*   **Role:** Extraction, normalization, deduplication, chunking, and ranking.
*   **Promise:** "I will turn your messy agent logs into a clean, searchable, and ranked ledger of past work on disk."
*   **Action:** Provides tools (`store`, `refs`, `rank`, `search`) to help the agent *find* what matters. It does not read the files for the agent; it tells the agent *where to look*.

### B. `memex` / `rmcp-memex` (The Semantic Search Add-on)
*   **Role:** Vector embedding and semantic retrieval of AICX chunks.
*   **Promise:** "I will let you search the AICX ledger by meaning instead of just keywords."
*   **Boundary:** Memex is strictly an *add-on* for fuzzy retrieval. It is not the "brain" of the system. It is a secondary index on top of the primary file-based store.

### C. The Agent's Own Perception (The Reader)
*   **Role:** Actually reading the file contents and applying them to the current task.
*   **Promise:** The agent must actively use tools (like `read_file`) to pull the referenced chunks from the AICX store into its active context window.
*   **Boundary:** `aicx` provides the map (`aicx_refs`, `aicx_search`); the agent must walk the territory. The agent's context window *is* its true working memory.

## 4. Recommended Actionable Changes

While this report serves as the framing baseline, the following specific updates are recommended for the next docs/positioning pass:

1.  **Rewrite `README.md` intro:**
    *   *Current:* "Memory extraction + context distillation for AI agent sessions."
    *   *Proposed:* "Timeline extraction and context indexing for AI agent sessions. `aicx` turns raw agent logs into a clean, searchable ledger, empowering new agents to quickly locate and re-enter past work."
2.  **Update MCP Tool Descriptions (`src/mcp.rs`):**
    *   Refine `aicx_search` and `aicx_refs` to emphasize they return *indexes* and *paths*, prompting the agent to read the actual files.
    *   *Note:* The smoke context mentioned "MCP lacks a direct `open/read chunk` tool". This is a critical gap. Relying on generic file reading is okay, but a dedicated tool to pull a specific chunk's content would solidify the "selective re-entry" workflow.
3.  **Deprecate "Vector Memory" phrase:** Replace instances of "vector memory" in `src/main.rs` help text and `COMMANDS.md` with "vector index" or "semantic search index".
4.  **Clarify `init` intent:** Ensure `aicx init` is described as "bootstrapping a localized context ledger" rather than "giving the agent memory".

## Conclusion
By adopting the "Perception over Memory" stance, AICX presents a more honest and reliable toolset. It shifts the burden of "understanding" away from a brittle storage layer and back to the agent's active reasoning, providing the perfect indexing tools to support that process.