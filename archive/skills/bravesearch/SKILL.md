---
name: brave-search
description: Search the web using Brave Search API. Use when you need web search results with better control than built-in WebSearch — supports language filtering, configurable result count, and returns structured results. Prefer this over WebSearch for research tasks.
---

# Brave Search Skill

## When to Use

Use this skill instead of `WebSearch` when:
- You need web search results for research or current information
- You want to control result count or language
- Built-in WebSearch is rate-limited or returning poor results

## How to Search

Run the Python script via Bash tool:

```bash
python3 ~/.claude/skills/bravesearch/brave_search.py "your search query"
```

### Options

- `--count N` or `-c N` — number of results (default: 8, max: 20)
- `--lang LANG` or `-l LANG` — search language code (e.g., `pl`, `en`, `de`)

### Examples

```bash
# Basic search
python3 ~/.claude/skills/bravesearch/brave_search.py "Apple SpeechAnalyzer API 2025"

# Polish language search, 5 results
python3 ~/.claude/skills/bravesearch/brave_search.py "transkrypcja mowy CoreML" -l pl -c 5

# Multiple word query
python3 ~/.claude/skills/bravesearch/brave_search.py "Rust FFI Swift bridging macOS"
```

## Output Format

Results are numbered with title, URL, and description:

```
[1] Title of Result
    https://example.com/page
    Description of the page content

[2] ...
```

News results (if available) are appended at the end.

## Important

- Always include source URLs in your response when using search results
- This skill uses the Brave Search API (free tier, 2000 queries/month)
- No dependencies beyond Python 3 stdlib (urllib, json)
- API key is embedded in the script — do not share the script publicly
