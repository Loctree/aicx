# Loctree CLI Command Reference

## Quick Reference

| Command | Purpose | Speed |
|---------|---------|-------|
| `loct --for-ai` | Codebase overview | ~500ms |
| `loct find <sym>` | Symbol search | ~280ms |
| `loct impact <file>` | Change impact | ~50ms |
| `loct slice <file>` | Dependency slice | ~80ms |
| `loct health` | Health report | ~370ms |
| `loct focus <dir>` | Directory analysis | ~60ms |

## Detailed Commands

### loct --for-ai

Get comprehensive codebase overview optimized for AI understanding.

```bash
loct --for-ai
```

**Output includes:**
- Project structure
- Entry points
- Key modules
- Health summary

### loct find

Search for symbol definitions with semantic matching.

```bash
loct find "MySymbol"           # Single symbol
loct find "A|B|C"              # Multi-query (OR)
loct find "handle.*Click"      # Pattern (transformed to multi-query)
```

**Options:**
- `--limit N` - Limit results (default: 50)
- `--json` - JSON output

### loct impact

Analyze what files would be affected by changes.

```bash
loct impact src/utils/helpers.ts
loct impact src/api/             # Directory impact
```

**Output:**
- Direct dependents
- Transitive dependents
- Risk assessment

### loct slice

Get minimal dependency context for a file.

```bash
loct slice src/components/Button.tsx
```

**Output:**
- Direct imports
- Transitive imports
- Suggested reading order

### loct health

Codebase health analysis.

```bash
loct health
loct health --json              # JSON output
```

**Detects:**
- Dead exports (unused code)
- Circular dependencies
- Duplicate exports (twins)
- Barrel chaos

### loct query

Low-level query interface.

```bash
loct query who-imports src/auth.ts     # What imports this?
loct query where-symbol MyClass        # Where is this symbol?
loct query why-import A.ts B.ts        # Why does A import B?
```

### loct focus

Analyze a specific directory.

```bash
loct focus src/api/
loct focus src/components/ --depth 2
```

### loct commands

List Tauri commands (for Tauri projects).

```bash
loct commands
loct commands --unused           # Find unused commands
```

## Output Formats

Most commands support:
- Default: Human-readable
- `--json`: JSON output
- `--for-ai`: AI-optimized format

## Environment Variables

- `LOCT_HOOK_LOG_FILE` - Hook log location (default: `$CLAUDE_LOCAL_DIR/logs/loct-hook.log`)

---

*See full documentation at https://github.com/Loctree/loctree-suite*
