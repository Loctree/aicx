#!/usr/bin/env python3
"""Executable gate for the AICX parser normative contract (C0A).

Validates, against docs/PARSER_NORMATIVE_CONTRACT.md:

  1. Field ownership matrix (normative_fields.toml): every donor field of
     session_record.v1 (schema 1.2.0) is classified into exactly one class;
     unclassified fields are rejected; heuristic/diagnostic/out_of_scope
     fields are forbidden in the kernel fingerprint; AICX extensions
     (UsageEvent, evidence_event_id, status contract) are present and
     normative.
  2. Raw-unit taxonomy (raw_unit_taxonomy.toml) + taxonomy fixture units:
     every fixture unit terminates in exactly one declared taxonomy kind.
  3. Parse-status truth table: contradictory states are rejected, valid
     states pass (contract section 3.3 rules).
  4. UsageEvent matrix: typed semantics, unknown-stays-unknown (never zero),
     coverage of cumulative/delta/snapshot, cache tokens, missing cost,
     model drift; invalid events are rejected.
  5. evidence_event_id derivation v1: append-stable, relocation-stable
     (path never enters the derivation), content-scoped mutation,
     duplicate-id failure.

Usage:
  contract_check.py --self-test
  contract_check.py --fields <normative_fields.toml> --taxonomy <raw_unit_taxonomy.toml>
                    [--fixtures <dir>]   (default: tests/fixtures/parser_engine/contract)

Exit code 0 = contract holds; 1 = violation (details on stderr).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path

FIELD_CLASSES = {"normative", "heuristic_projection", "diagnostic", "out_of_scope"}
PROVENANCE_GRADES = {"donor_spec", "donor_adapter", "aicx_history", "recon"}
VISIBLE_COMPLETENESS = {"complete_visible", "partial_visible", "fatal"}
COUNTER_SEMANTICS = {"snapshot", "delta", "cumulative"}
REQUIRED_AGENTS = {"codex", "claude", "gemini", "grok", "junie"}

# Donor field inventory — session_record.v1, schema 1.2.0 (donor spec sections
# 2/3/3a-3d/7). A donor field missing from the matrix, or a matrix entry not in
# this inventory, is a contract failure. Extending the donor spec means
# extending BOTH this list and the matrix in the same commit.
DONOR_FIELD_PATHS = frozenset({
    "schema", "schema_version", "map_id", "session_id", "generated_at",
    "generator.name", "generator.version", "generator.adapter",
    "provenance.agent", "provenance.model", "provenance.cli_version",
    "provenance.cwd", "provenance.branch", "provenance.started_at",
    "provenance.ended_at", "provenance.duration_seconds",
    "provenance.duration_span_seconds", "provenance.active_duration_seconds",
    "provenance.duration_gap_cap_seconds", "provenance.original_jsonl_path",
    "provenance.original_jsonl_hash", "provenance.original_jsonl_bytes",
    "provenance.bundle_relative_path",
    "intent.summary", "intent.body", "intent.body_source", "intent.body_chars",
    "intent.acceptance_criteria_raw", "intent.out_of_scope_raw",
    "dispatched_via.skill_name", "dispatched_via.skill_source_path",
    "dispatched_via.dispatch_hash", "dispatched_via.dispatch_signals_detected",
    "dispatched_via.dispatch_prompt_turn_idx",
    "dispatched_via.is_dispatched_workflow",
    "segments[].segment_id", "segments[].source_project",
    "segments[].topic_project_candidate", "segments[].project_mismatch",
    "segments[].cwd", "segments[].branch", "segments[].started_at",
    "segments[].ended_at", "segments[].turn_range", "segments[].sub_intent",
    "segments[].decision_candidates[].turn_idx",
    "segments[].decision_candidates[].timestamp",
    "segments[].decision_candidates[].text_excerpt",
    "segments[].decision_candidates[].kind",
    "segments[].decision_candidates[].confidence",
    "segments[].footprint.tool_call_counts",
    "segments[].footprint.files_referenced",
    "segments[].footprint.files_referenced_count",
    "segments[].footprint.files_modified",
    "segments[].footprint.files_modified_count",
    "segments[].footprint.files_touched",
    "segments[].footprint.files_touched_count",
    "segments[].footprint.commands_executed",
    "segments[].footprint.git_diff_stat",
    "segments[].outcome.verdict", "segments[].outcome.agent_outcome",
    "segments[].outcome.tb_verification_state",
    "segments[].outcome.deliverables", "segments[].outcome.gates",
    "segments[].outcome.last_assistant_excerpts",
    "segments[].outcome.verdict_confidence",
    "segments[].boundary[].kind", "segments[].boundary[].count",
    "segments[].boundary[].first_turn_index", "segments[].boundary[].note",
    "segments[].boundary[].recovered_via",
    "skill_invocations[].turn_idx", "skill_invocations[].skill_name",
    "skill_invocations[].skill_source_path", "skill_invocations[].payload_hash",
    "skill_invocations[].payload_bytes", "skill_invocations[].first_invoked_at",
    "skill_invocations[].detection_marker",
    "parser_coverage.raw_line_count", "parser_coverage.consumed_count",
    "parser_coverage.skipped_count", "parser_coverage.unreadable_count",
    "parser_coverage.unsupported_count", "parser_coverage.consumed_ranges",
    "parser_coverage.consumed_by_kind",
    "parser_coverage.skipped_lines[].line_no",
    "parser_coverage.skipped_lines[].reason",
    "parser_coverage.skipped_lines[].detail",
    "parser_coverage.skipped_lines[].bytes",
    "parser_coverage.parse_status",
    "parser_coverage.warnings[].kind", "parser_coverage.warnings[].count",
    "parser_coverage.warnings[].first_line_no",
    "parser_coverage.warnings[].samples",
    "chat.preserved", "chat.preservation_policy", "chat.turn_count",
    "chat.turns[].turn_idx", "chat.turns[].role", "chat.turns[].timestamp",
    "chat.turns[].kind", "chat.turns[].text_preview", "chat.turns[].text_hash",
    "chat.turns[].text_chars", "chat.turns[].tool_name",
    "chat.turns[].tool_input_summary", "chat.turns[].tool_output_summary",
    "chat.turns[].segment_id", "chat.turns[].dispatch_signals_detected",
    "chat.turns[].raw_line_nos",
})

# AICX normative extensions that MUST exist, be normative, and be fingerprinted.
AICX_EXTENSION_PATHS = frozenset({
    "usage_events[]", "evidence_event_id", "visible_completeness",
    "boundary_flags.opaque_reasoning_present",
    "boundary_flags.unsupported_visible_event",
})


def fail(errors: list[str], where: str) -> None:
    if errors:
        print(f"CONTRACT VIOLATION [{where}]:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        sys.exit(1)


# --------------------------------------------------------------------------
# 1. Field ownership matrix
# --------------------------------------------------------------------------

def check_fields(doc: dict) -> list[str]:
    errors: list[str] = []
    if doc.get("schema") != "aicx.parser.normative_fields.v1":
        errors.append(f"unexpected fields schema: {doc.get('schema')!r}")
    if doc.get("donor_schema_version") != "1.2.0":
        errors.append("donor_schema_version drifted from the frozen 1.2.0 oracle")

    seen: dict[str, dict] = {}
    for entry in doc.get("field", []):
        path = entry.get("path", "<missing path>")
        if path in seen:
            errors.append(f"duplicate classification for donor field {path!r}")
        seen[path] = entry
        errors.extend(check_field_entry(entry, donor=True))

    classified = set(seen)
    for missing in sorted(DONOR_FIELD_PATHS - classified):
        errors.append(f"UNCLASSIFIED donor field: {missing!r}")
    for phantom in sorted(classified - DONOR_FIELD_PATHS):
        errors.append(
            f"matrix classifies {phantom!r} which is not in the donor inventory"
        )

    ext_seen: dict[str, dict] = {}
    for entry in doc.get("aicx_field", []):
        path = entry.get("path", "<missing path>")
        if path in ext_seen:
            errors.append(f"duplicate AICX extension entry {path!r}")
        ext_seen[path] = entry
        errors.extend(check_field_entry(entry, donor=False))
        if entry.get("class") != "normative" or entry.get("kernel_fingerprint") is not True:
            errors.append(
                f"AICX extension {path!r} must be normative + kernel_fingerprint=true"
            )
    for missing in sorted(AICX_EXTENSION_PATHS - set(ext_seen)):
        errors.append(f"missing required AICX extension entry: {missing!r}")
    return errors


def check_field_entry(entry: dict, donor: bool) -> list[str]:
    errors: list[str] = []
    path = entry.get("path", "<missing path>")
    cls = entry.get("class")
    if cls not in FIELD_CLASSES:
        errors.append(f"{path}: class {cls!r} outside {sorted(FIELD_CLASSES)}")
    if not entry.get("reason", "").strip():
        errors.append(f"{path}: empty reason")
    if not entry.get("owner", "").strip():
        errors.append(f"{path}: empty owner")
    if "kernel_fingerprint" not in entry or not isinstance(entry["kernel_fingerprint"], bool):
        errors.append(f"{path}: kernel_fingerprint must be an explicit bool")
    elif entry["kernel_fingerprint"] and cls != "normative":
        errors.append(
            f"{path}: non-normative class {cls!r} is FORBIDDEN in the kernel fingerprint"
        )
    return errors


# --------------------------------------------------------------------------
# 2. Raw-unit taxonomy + fixture units
# --------------------------------------------------------------------------

def check_taxonomy(doc: dict) -> list[str]:
    errors: list[str] = []
    if doc.get("schema") != "aicx.parser.raw_unit_taxonomy.v1":
        errors.append(f"unexpected taxonomy schema: {doc.get('schema')!r}")
    agents = {a.get("name"): a for a in doc.get("agent", [])}
    for required in sorted(REQUIRED_AGENTS - set(agents)):
        errors.append(f"taxonomy missing required agent {required!r}")
    for name, agent in agents.items():
        kinds = agent.get("kind", [])
        if not kinds:
            errors.append(f"agent {name!r} declares no kinds")
        seen_keys: set[tuple] = set()
        for kind in kinds:
            key = (
                kind.get("name"), kind.get("level"), kind.get("parent"),
                kind.get("container_file"),
            )
            if key in seen_keys:
                errors.append(f"agent {name!r}: duplicate kind declaration {key}")
            seen_keys.add(key)
            if kind.get("level") not in {"physical", "logical"}:
                errors.append(f"{name}/{kind.get('name')}: bad level {kind.get('level')!r}")
            if kind.get("level") == "logical" and not kind.get("parent"):
                errors.append(f"{name}/{kind.get('name')}: logical kind without parent")
            if kind.get("provenance") not in PROVENANCE_GRADES:
                errors.append(
                    f"{name}/{kind.get('name')}: provenance {kind.get('provenance')!r} "
                    f"outside {sorted(PROVENANCE_GRADES)}"
                )
            if not str(kind.get("locator", "")).strip():
                errors.append(f"{name}/{kind.get('name')}: missing locator")
            if not str(kind.get("description", "")).strip():
                errors.append(f"{name}/{kind.get('name')}: missing description")
            errors.extend(check_match_rule(name, kind))
    return errors


def check_match_rule(agent: str, kind: dict) -> list[str]:
    match = kind.get("match")
    if not isinstance(match, dict) or not match:
        return [f"{agent}/{kind.get('name')}: missing match rule"]
    known = {"any", "field", "equals", "equals_any", "has_field", "has_fields", "lacks_field"}
    unknown = set(match) - known
    if unknown:
        return [f"{agent}/{kind.get('name')}: unknown match keys {sorted(unknown)}"]
    if "field" in match and not ("equals" in match or "equals_any" in match):
        return [f"{agent}/{kind.get('name')}: field without equals/equals_any"]
    return []


def rule_matches(match: dict, unit: dict) -> bool:
    if match.get("any"):
        return True
    ok = True
    if "field" in match:
        value = unit.get(match["field"])
        if "equals" in match:
            ok = ok and value == match["equals"]
        if "equals_any" in match:
            ok = ok and value in match["equals_any"]
    if "has_field" in match:
        ok = ok and match["has_field"] in unit
    if "has_fields" in match:
        ok = ok and all(f in unit for f in match["has_fields"])
    if "lacks_field" in match:
        ok = ok and match["lacks_field"] not in unit
    return ok


def classify_unit(agent: dict, level: str, parent: str | None,
                  container_file: str | None, unit: dict) -> list[str]:
    """Return the names of all taxonomy kinds the unit terminates in."""
    hits = []
    for kind in agent.get("kind", []):
        if kind.get("level") != level:
            continue
        if kind.get("parent") != parent:
            continue
        if kind.get("container_file") != container_file:
            continue
        if rule_matches(kind.get("match", {}), unit):
            hits.append(kind["name"])
    return hits


def check_taxonomy_units(taxonomy: dict, units_doc: dict) -> list[str]:
    errors: list[str] = []
    if units_doc.get("schema") != "aicx.parser.taxonomy_units.v1":
        errors.append(f"unexpected taxonomy_units schema: {units_doc.get('schema')!r}")
    agents = {a.get("name"): a for a in taxonomy.get("agent", [])}
    per_agent_count: dict[str, int] = {}
    for i, unit in enumerate(units_doc.get("unit", [])):
        label = f"unit[{i}] ({unit.get('agent')}/{unit.get('expected_kind')})"
        agent = agents.get(unit.get("agent"))
        if agent is None:
            errors.append(f"{label}: unknown agent")
            continue
        per_agent_count[unit["agent"]] = per_agent_count.get(unit["agent"], 0) + 1
        try:
            sample = json.loads(unit["sample"])
        except (KeyError, json.JSONDecodeError) as exc:
            errors.append(f"{label}: unparsable sample: {exc}")
            continue
        hits = classify_unit(
            agent, unit.get("level"), unit.get("parent"),
            unit.get("container_file"), sample,
        )
        if len(hits) != 1:
            errors.append(
                f"{label}: terminates in {len(hits)} kinds {hits} — must be exactly one"
            )
        elif hits[0] != unit.get("expected_kind"):
            errors.append(f"{label}: matched {hits[0]!r}, expected {unit.get('expected_kind')!r}")
    for name in sorted(REQUIRED_AGENTS):
        if per_agent_count.get(name, 0) < 3:
            errors.append(
                f"agent {name!r} has {per_agent_count.get(name, 0)} fixture units (< 3)"
            )
    return errors


# --------------------------------------------------------------------------
# 3. Parse-status truth table
# --------------------------------------------------------------------------

def status_case_errors(case: dict) -> list[str]:
    errs: list[str] = []
    vc = case.get("visible_completeness")
    if vc not in VISIBLE_COMPLETENESS:
        errs.append(f"visible_completeness {vc!r} outside {sorted(VISIBLE_COMPLETENESS)}")
        return errs
    malformed = case.get("malformed_tail_present", False)
    lost = case.get("visible_event_lost", False)
    opaque = case.get("opaque_reasoning_present", False)
    unsupported = case.get("unsupported_visible_event", False)
    warnings = case.get("warnings_count", 0)
    projected = case.get("model_projected", True)
    # Rule 1: malformed tail forbids complete_visible.
    if malformed and vc == "complete_visible":
        errs.append("malformed tail present but status claims complete_visible")
    # Rule 2: partial_visible needs concrete visible loss; opaque alone never counts.
    if vc == "partial_visible" and not (malformed or lost):
        detail = "opaque reasoning alone" if opaque else "no visible loss recorded"
        errs.append(f"partial_visible without concrete visible loss ({detail})")
    # Rule 3: preserved-unsupported requires a warning record.
    if unsupported and warnings < 1:
        errs.append("unsupported_visible_event without a warning/boundary record")
    # Rule 4: fatal forbids projection/ingest.
    if vc == "fatal" and projected:
        errs.append("fatal status but a model was projected/ingested")
    return errs


def check_truth_table(doc: dict) -> list[str]:
    errors: list[str] = []
    if doc.get("schema") != "aicx.parser.parse_status_truth_table.v1":
        errors.append(f"unexpected truth-table schema: {doc.get('schema')!r}")
    cases = doc.get("case", [])
    if len(cases) < 8:
        errors.append(f"truth table too small: {len(cases)} cases (< 8)")
    saw_opaque_complete = False
    for case in cases:
        name = case.get("name", "<unnamed>")
        errs = status_case_errors(case)
        expect = case.get("expect")
        if expect == "valid" and errs:
            errors.extend(f"case {name!r} expected valid, rejected: {e}" for e in errs)
        elif expect == "invalid" and not errs:
            errors.append(f"case {name!r} expected invalid (contradictory) but passed")
        elif expect not in {"valid", "invalid"}:
            errors.append(f"case {name!r}: expect must be valid|invalid")
        if (case.get("visible_completeness") == "complete_visible"
                and case.get("opaque_reasoning_present") and expect == "valid"):
            saw_opaque_complete = True
    if not saw_opaque_complete:
        errors.append(
            "truth table missing the load-bearing case: "
            "complete_visible + opaque_reasoning_present must be VALID"
        )
    return errors


# --------------------------------------------------------------------------
# 4. UsageEvent matrix
# --------------------------------------------------------------------------

def _component_errors(name: str, value) -> list[str]:
    if value == "unknown":
        return []
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return [f"{name}: must be a non-negative number or the string 'unknown'"]
    if value < 0:
        return [f"{name}: below zero"]
    return []


def usage_event_errors(event: dict) -> list[str]:
    errs: list[str] = []
    name = event.get("name", "<unnamed>")
    for required in ("provider", "model", "counter_semantics", "session", "timestamp"):
        if required not in event:
            errs.append(f"{name}: missing {required}")
    if event.get("counter_semantics") not in COUNTER_SEMANTICS:
        errs.append(
            f"{name}: counter_semantics {event.get('counter_semantics')!r} "
            f"outside {sorted(COUNTER_SEMANTICS)}"
        )
    tokens = event.get("tokens", {})
    for comp in ("input", "output", "reasoning", "cache_read", "cache_creation"):
        if comp not in tokens:
            errs.append(f"{name}: tokens.{comp} missing (use 'unknown', never omit or zero-fill)")
        else:
            errs.extend(f"{name}: tokens.{e}" for e in _component_errors(comp, tokens[comp]))
    cost = event.get("cost")
    if cost is None:
        errs.append(f"{name}: cost missing (use value='unknown')")
    elif isinstance(cost, dict):
        if cost.get("value") == "unknown":
            pass  # explicitly unreported — the only legal unknown encoding
        elif "amount" in cost:
            errs.extend(f"{name}: cost.{e}" for e in _component_errors("amount", cost["amount"]))
            if not str(cost.get("currency", "")).strip():
                errs.append(f"{name}: reported cost amount without currency")
        else:
            errs.append(f"{name}: cost must carry amount+currency or value='unknown'")
        if cost.get("value") not in (None, "unknown") and not isinstance(cost.get("value"), str):
            errs.append(f"{name}: cost.value={cost.get('value')!r} — unknown must never be a number")
    else:
        errs.append(f"{name}: cost must be a table, not a bare scalar")
    return errs


def check_usage_matrix(doc: dict) -> list[str]:
    errors: list[str] = []
    if doc.get("schema") != "aicx.parser.usage_matrix.v1":
        errors.append(f"unexpected usage schema: {doc.get('schema')!r}")
    events = doc.get("event", [])
    for event in events:
        errs = usage_event_errors(event)
        errors.extend(f"valid event rejected: {e}" for e in errs)
    for bad in doc.get("invalid_event", []):
        if not usage_event_errors(bad):
            errors.append(
                f"invalid event {bad.get('name')!r} passed validation "
                f"(declared violation: {bad.get('violation')!r})"
            )
    semantics = {e.get("counter_semantics") for e in events}
    for needed in COUNTER_SEMANTICS:
        if needed not in semantics:
            errors.append(f"matrix lacks a counter_semantics={needed!r} event")
    if not any(
        isinstance(e.get("tokens", {}).get("cache_read"), (int, float)) for e in events
    ):
        errors.append("matrix lacks an event with known cache tokens")
    if not any(e.get("cost", {}).get("value") == "unknown" for e in events):
        errors.append("matrix lacks a missing-cost (unknown) event")
    by_session: dict[str, set[str]] = {}
    for e in events:
        by_session.setdefault(e.get("session", ""), set()).add(e.get("model", ""))
    if not any(len(models) >= 2 for models in by_session.values()):
        errors.append("matrix lacks a mid-session model-drift pair")
    return errors


# --------------------------------------------------------------------------
# 5. evidence_event_id derivation v1
# --------------------------------------------------------------------------

def derive_ids_with_locators(
    units: list[tuple[str, str, str]], agent: str, session_id: str
) -> list[str]:
    """Derivation v1 (contract section 5.1) over (locator, kind, raw_bytes)
    triples. Takes NO filesystem path — relocation stability holds by
    construction. Duplicate ids (same locator + same content) are a fatal
    accounting violation; they can only arise from a broken logical locator,
    never from physical ordinals."""
    ids = []
    for locator, kind, raw in units:
        content_hash = hashlib.sha256(raw.encode("utf-8")).hexdigest()[:16]
        ids.append(f"ev1:{agent}:{session_id}:{locator}:{kind}:{content_hash}")
    if len(set(ids)) != len(ids):
        raise ValueError("duplicate evidence_event_id — fatal accounting violation")
    return ids


def derive_ids(lines: list[str], agent: str) -> list[str]:
    """Physical-ordinal form of derivation v1 for line-oriented sources."""
    session_id = None
    for line in lines:
        obj = json.loads(line)
        if obj.get("type") == "session_meta":
            session_id = obj.get("payload", {}).get("id")
            break
    if not session_id:
        raise ValueError("no session identity in the unit stream")
    units = [
        (f"{ordinal:06d}", json.loads(line).get("type", "unknown"), line)
        for ordinal, line in enumerate(lines, start=1)
    ]
    return derive_ids_with_locators(units, agent, session_id)


def read_units(path: Path) -> list[str]:
    return [ln for ln in path.read_text(encoding="utf-8").splitlines() if ln.strip()]


def check_identity_fixtures(fixtures: Path) -> list[str]:
    errors: list[str] = []
    base = read_units(fixtures / "identity_base.jsonl")
    appended = read_units(fixtures / "identity_append.jsonl")
    mutated = read_units(fixtures / "identity_mutated.jsonl")
    if appended[: len(base)] != base:
        errors.append("identity_append.jsonl is not an append of identity_base.jsonl")
    if len(mutated) != len(base) or sum(a != b for a, b in zip(base, mutated)) != 1:
        errors.append("identity_mutated.jsonl must differ from base in exactly one unit")

    ids_base = derive_ids(base, "codex")
    ids_append = derive_ids(appended, "codex")
    ids_mutated = derive_ids(mutated, "codex")

    # Invariant 1: append preserves every prior id byte-for-byte.
    if ids_append[: len(ids_base)] != ids_base:
        errors.append("append changed a prior evidence_event_id")
    # Invariant 3: mutating one raw unit changes only its id.
    diffs = [i for i, (a, b) in enumerate(zip(ids_base, ids_mutated)) if a != b]
    mutated_ordinals = [i for i, (a, b) in enumerate(zip(base, mutated)) if a != b]
    if diffs != mutated_ordinals:
        errors.append(
            f"mutation changed ids at ordinals {diffs}, expected exactly {mutated_ordinals}"
        )
    # Invariant 4: no absolute path may appear in any id.
    for eid in ids_base + ids_append + ids_mutated:
        if "/Users" in eid or "/Volumes" in eid or eid.count("/") > 0:
            errors.append(f"evidence_event_id embeds a path: {eid}")
    # Invariant 5: duplicates fail — a broken logical locator (same locator,
    # same content twice) must raise, constructively.
    try:
        derive_ids_with_locators(
            [("step:1:tool", "block_update", base[1]),
             ("step:1:tool", "block_update", base[1])],
            "junie", "s-dup",
        )
    except ValueError:
        pass
    else:
        errors.append("duplicate-locator stream did not raise — duplicate ids must fail")
    return errors


# --------------------------------------------------------------------------
# Self-test — the gate checks its own teeth
# --------------------------------------------------------------------------

def self_test() -> list[str]:
    errors: list[str] = []

    # Unclassified donor field must be detected.
    doc = {
        "schema": "aicx.parser.normative_fields.v1",
        "donor_schema_version": "1.2.0",
        "field": [{
            "path": "session_id", "class": "normative", "owner": "C1 kernel",
            "kernel_fingerprint": True, "reason": "x",
        }],
        "aicx_field": [],
    }
    errs = check_fields(doc)
    if not any("UNCLASSIFIED" in e for e in errs):
        errors.append("self-test: unclassified donor field not detected")

    # Heuristic field in the kernel fingerprint must be rejected.
    entry = {
        "path": "intent.summary", "class": "heuristic_projection",
        "owner": "A1 overlay", "kernel_fingerprint": True, "reason": "x",
    }
    if not any("FORBIDDEN" in e for e in check_field_entry(entry, donor=True)):
        errors.append("self-test: heuristic-in-fingerprint not rejected")

    # Contradictory status must be rejected; opaque+complete must pass.
    bad = {
        "visible_completeness": "complete_visible", "malformed_tail_present": True,
        "warnings_count": 1,
    }
    if not status_case_errors(bad):
        errors.append("self-test: malformed-but-complete contradiction not rejected")
    good = {
        "visible_completeness": "complete_visible",
        "opaque_reasoning_present": True, "warnings_count": 1,
    }
    if status_case_errors(good):
        errors.append("self-test: complete_visible + opaque wrongly rejected")
    opaque_partial = {
        "visible_completeness": "partial_visible",
        "opaque_reasoning_present": True, "warnings_count": 1,
    }
    if not status_case_errors(opaque_partial):
        errors.append("self-test: opaque-only partial_visible not rejected")

    # Usage: bare-scalar cost and negative tokens must be rejected.
    bad_usage = {
        "name": "t", "session": "s", "provider": "p", "model": "m",
        "counter_semantics": "delta", "timestamp": "unknown",
        "tokens": {"input": -1, "output": 0, "reasoning": "unknown",
                   "cache_read": "unknown", "cache_creation": "unknown"},
        "cost": 0,
    }
    errs = usage_event_errors(bad_usage)
    if not any("below zero" in e for e in errs):
        errors.append("self-test: negative token component not rejected")
    if not any("bare scalar" in e for e in errs):
        errors.append("self-test: fabricated bare-zero cost not rejected")

    # Identity: duplicate units must raise; append must be stable.
    lines = [
        '{"type":"session_meta","payload":{"id":"s-self"}}',
        '{"type":"response_item","payload":{"type":"message","content":"a"}}',
    ]
    ids1 = derive_ids(lines, "codex")
    ids2 = derive_ids(lines + ['{"type":"response_item","payload":{"type":"message","content":"b"}}'], "codex")
    if ids2[:2] != ids1:
        errors.append("self-test: append changed prior ids")
    try:
        derive_ids_with_locators(
            [("000001", "message", lines[1]), ("000001", "message", lines[1])],
            "codex", "s-self",
        )
    except ValueError:
        pass
    else:
        errors.append("self-test: duplicate locator stream did not raise")

    # Taxonomy: ambiguous double-match must be reported as != 1 hits.
    agent = {"kind": [
        {"name": "a", "level": "physical", "parent": None, "container_file": None,
         "match": {"has_field": "x"}},
        {"name": "b", "level": "physical", "parent": None, "container_file": None,
         "match": {"has_field": "x"}},
    ]}
    if len(classify_unit(agent, "physical", None, None, {"x": 1})) == 1:
        errors.append("self-test: ambiguous taxonomy match not surfaced")
    return errors


# --------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--fields", type=Path)
    parser.add_argument("--taxonomy", type=Path)
    parser.add_argument(
        "--fixtures", type=Path,
        default=Path("tests/fixtures/parser_engine/contract"),
    )
    args = parser.parse_args()

    if args.self_test:
        fail(self_test(), "self-test")
        print("contract_check self-test: OK")
        return 0

    if not args.fields or not args.taxonomy:
        parser.error("--fields and --taxonomy are required (or use --self-test)")

    fields_doc = tomllib.loads(args.fields.read_text(encoding="utf-8"))
    taxonomy_doc = tomllib.loads(args.taxonomy.read_text(encoding="utf-8"))
    fail(check_fields(fields_doc), "field ownership matrix")
    fail(check_taxonomy(taxonomy_doc), "raw-unit taxonomy")

    fixtures = args.fixtures
    units_doc = tomllib.loads((fixtures / "taxonomy_units.toml").read_text(encoding="utf-8"))
    fail(check_taxonomy_units(taxonomy_doc, units_doc), "taxonomy fixture units")
    truth_doc = tomllib.loads(
        (fixtures / "parse_status_truth_table.toml").read_text(encoding="utf-8")
    )
    fail(check_truth_table(truth_doc), "parse-status truth table")
    usage_doc = tomllib.loads((fixtures / "usage_matrix.toml").read_text(encoding="utf-8"))
    fail(check_usage_matrix(usage_doc), "usage matrix")
    fail(check_identity_fixtures(fixtures), "evidence_event_id derivation")

    n_fields = len(fields_doc.get("field", []))
    n_ext = len(fields_doc.get("aicx_field", []))
    n_kinds = sum(len(a.get("kind", [])) for a in taxonomy_doc.get("agent", []))
    print(
        f"contract holds: {n_fields} donor fields classified, {n_ext} AICX extensions, "
        f"{n_kinds} taxonomy kinds, truth table + usage matrix + identity fixtures green"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
