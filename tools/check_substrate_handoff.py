#!/usr/bin/env python3
"""Validate the parser-transplant to substrate-makieta handoff packet.

The checker intentionally stays stdlib-only so it can run in recovery gates
without bootstrapping project dependencies.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


PLACEHOLDER_RE = re.compile(r"DO UZUPE|PENDING C9N|TODO|TBD|do douzu", re.IGNORECASE)
HEX_RE = re.compile(r"`?([0-9a-f]{12,64})`?", re.IGNORECASE)


def section(text: str, heading: str) -> str:
    pattern = re.compile(
        rf"^##\s+\d+\.\s+{re.escape(heading)}\b.*?(?=^##\s+\d+\.|\Z)",
        re.MULTILINE | re.DOTALL,
    )
    match = pattern.search(text)
    return match.group(0) if match else ""


def has_all(text: str, *needles: str) -> bool:
    haystack = text.lower()
    return all(needle.lower() in haystack for needle in needles)


def valid_store_revision(store_section: str) -> bool:
    value_line = ""
    for line in store_section.splitlines():
        if "current value" in line.lower() and "fixture corpus" in line.lower():
            value_line = line
            break
    if not value_line or PLACEHOLDER_RE.search(value_line):
        return False
    values = [m.group(1) for m in HEX_RE.finditer(value_line)]
    return any(len(value) >= 32 for value in values)


def validate(contract_text: str, packet_text: str) -> list[str]:
    missing: list[str] = []

    if "Handoff packet from transplant to makieta" not in contract_text:
        missing.append("contract section: Handoff packet from transplant to makieta")

    commit = section(packet_text, "Converged AICX commit")
    cli = section(packet_text, "Canonical CLI matrix")
    schemas = section(packet_text, "SessionModel / schema versions")
    evidence = section(packet_text, "evidence-id derivation")
    store = section(packet_text, "store_revision")
    cards = section(packet_text, "Canonical card fields dostępne dlo A1")
    verdicts = section(packet_text, "Werdykty i raporty C8/C8P/C8N")
    plugin = section(packet_text, "C7H plugin version + activation proof")
    awave = section(packet_text, "Instrukcjo dlo fali A")

    if not commit or not has_all(commit, "converged commit", "fix/aicx-daily-usefulness"):
        missing.append("converged AICX commit")
    elif not re.search(r"converged commit:\s*`[0-9a-f]{40}`", commit, re.IGNORECASE):
        missing.append("converged AICX commit: 40-hex value")

    if not cli or not has_all(cli, "aicx extract {codex|claude|gemini|grok|junie}", "legacy_flag_grammar"):
        missing.append("canonical CLI matrix")

    if not schemas or not has_all(schemas, "aicx.store.canonical_card.v3", "card.v2"):
        missing.append("SessionModel/schema versions")

    if not evidence or not has_all(evidence, "evidence_event_id", "derivation v1"):
        missing.append("evidence-id derivation version")

    if not store or not has_all(store, "canonical_chunks.rs::store_revision", "current value", "fixture corpus"):
        missing.append("store_revision derivation + fixture corpus field")
    elif not valid_store_revision(store):
        missing.append("store_revision current fixture corpus value")

    if not cards or not has_all(
        cards,
        "evidence_event_id",
        "UsageEvent",
        "producer_version",
        "attribution_version",
        "store_revision",
        "chunk:",
        "session:",
    ):
        missing.append("canonical card fields available to A1")

    if not plugin or not has_all(plugin, "plugin:", "activation proof", "PASS"):
        missing.append("C7H plugin version + activation proof")

    if not verdicts or not has_all(verdicts, "C8", "C8P", "C8N", "Raport"):
        missing.append("C8/C8P/C8N verdicts and reports")
    elif not all(re.search(token, verdicts) for token in (r"C8\b", r"C8P\b", r"C8N\b")):
        missing.append("C8/C8P/C8N verdict entries")

    if not awave or not has_all(awave, "Re-run structural intake", "converged commit"):
        missing.append("A-wave instruction: re-run structural intake from verified converged commit")

    return missing


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--contract", required=True, type=Path)
    parser.add_argument("--packet", required=True, type=Path)
    args = parser.parse_args()

    contract_text = args.contract.read_text(encoding="utf-8")
    packet_text = args.packet.read_text(encoding="utf-8")
    missing = validate(contract_text, packet_text)
    if missing:
        print("FAIL substrate handoff packet incomplete", file=sys.stderr)
        for item in missing:
            print(f"- missing: {item}", file=sys.stderr)
        return 2
    print("PASS substrate handoff packet complete")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())