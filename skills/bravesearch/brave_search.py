#!/usr/bin/env python3
"""Brave Search API client for Claude Code skill.

Usage:
    python3 brave_search.py "search query" [--count N] [--lang LANG]

Created by M&K (c)2026 VetCoders
"""

import json
import os
import sys
import urllib.request
import urllib.parse
import urllib.error
from typing import Optional

API_KEY = os.environ.get("BRAVE_API_KEY", "")
API_URL = "https://api.search.brave.com/res/v1/web/search"


def search(query: str, count: int = 8, lang: Optional[str] = None) -> dict:
    if not API_KEY:
        return {"error": "BRAVE_API_KEY environment variable is not set"}
    params = {"q": query, "count": str(count)}
    if lang:
        params["search_lang"] = lang

    url = f"{API_URL}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(url, headers={
        "Accept": "application/json",
        "Accept-Encoding": "identity",
        "X-Subscription-Token": API_KEY,
    })

    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}"}
    except Exception as e:
        return {"error": str(e)}


def format_results(data: dict) -> str:
    if "error" in data:
        return f"Error: {data['error']}"

    lines = []
    web = data.get("web", {}).get("results", [])
    if not web:
        return "No results found."

    for i, r in enumerate(web, 1):
        title = r.get("title", "No title")
        url = r.get("url", "")
        desc = r.get("description", "No description")
        lines.append(f"[{i}] {title}")
        lines.append(f"    {url}")
        lines.append(f"    {desc}")
        lines.append("")

    # Append news if present
    news = data.get("news", {}).get("results", [])
    if news:
        lines.append("--- News ---")
        for r in news[:3]:
            lines.append(f"  - {r.get('title', '')} ({r.get('url', '')})")
        lines.append("")

    return "\n".join(lines)


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser(description="Brave Search")
    parser.add_argument("query", nargs="+", help="Search query")
    parser.add_argument("--count", "-c", type=int, default=8)
    parser.add_argument("--lang", "-l", default=None)
    args = parser.parse_args()

    query = " ".join(args.query)
    data = search(query, count=args.count, lang=args.lang)
    print(format_results(data))
