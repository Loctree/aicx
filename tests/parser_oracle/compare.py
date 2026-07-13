#!/usr/bin/env python3
"""Manifest validation and differential comparison for parser replacement C0."""

from __future__ import annotations

import argparse
import copy
import json
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
SUPPORTED_AGENTS = {"codex", "grok", "claude", "gemini", "junie"}
DONOR_AGENTS = {"codex", "grok", "claude", "gemini"}
REQUIRED_CONTRACT_SECTIONS = (
    "## Ownership and source of truth",
    "## Replacement flow",
    "## Raw-unit accounting",
    "## Parse status and boundaries",
    "## Usage and evidence identity",
    "## Determinism",
    "## Differential oracle policy",
    "## Public Loctree boundary",
    "## Benchmark contract",
    "## Legacy deletion contract",
)


class OracleError(RuntimeError):
    """A stable user-facing harness failure."""


@dataclass(frozen=True)
class Case:
    id: str
    agent: str
    fixture: Path
    expected: Path
    oracle_kind: str
    oracle_command: str | None
    exact_fields: tuple[str, ...]
    heuristic_assertions: tuple[dict[str, Any], ...]


def load_toml(path: Path) -> dict[str, Any]:
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise OracleError(f"cannot read TOML {path}: {exc}") from exc


def repo_path(raw: str) -> Path:
    path = Path(raw)
    return path if path.is_absolute() else REPO_ROOT / path


def validate_contract(path: Path) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise OracleError(f"cannot read engine contract {path}: {exc}") from exc
    missing = [section for section in REQUIRED_CONTRACT_SECTIONS if section not in text]
    if missing:
        raise OracleError(f"engine contract missing required section: {missing[0]}")
    required_refs = (
        "PARSER_NORMATIVE_CONTRACT.md",
        "tests/parser_oracle/normative_fields.toml",
        "consumed(kind) XOR skipped(reason)",
        "aicx::api::Aicx",
    )
    for reference in required_refs:
        if reference not in text:
            raise OracleError(f"engine contract missing cross-reference: {reference}")


def validate_normative_matrix(path: Path) -> None:
    data = load_toml(path)
    fields = data.get("field") or data.get("fields")
    if not isinstance(fields, list) or not fields:
        raise OracleError(f"normative matrix has no [[field]] entries: {path}")
    allowed = {"normative", "heuristic_projection", "diagnostic", "out_of_scope"}
    for index, field in enumerate(fields):
        if not isinstance(field, dict):
            raise OracleError(f"normative matrix field[{index}] is not a table")
        name = field.get("path") or field.get("name")
        classification = field.get("classification") or field.get("class")
        if not isinstance(name, str) or not name:
            raise OracleError(f"normative matrix field[{index}] has no path")
        if classification not in allowed:
            raise OracleError(
                f"normative matrix field[{index}] {name!r} has invalid classification {classification!r}"
            )


def parse_manifest(path: Path, *, require_normative_matrix: bool = False) -> list[Case]:
    data = load_toml(path)
    if data.get("schema_version") != 1:
        raise OracleError("manifest schema_version must be 1")
    raw_cases = data.get("case")
    if not isinstance(raw_cases, list) or not raw_cases:
        raise OracleError("manifest requires at least one [[case]]")

    cases: list[Case] = []
    ids: set[str] = set()
    agents: set[str] = set()
    for index, item in enumerate(raw_cases):
        if not isinstance(item, dict):
            raise OracleError(f"case[{index}] must be a table")
        for required in (
            "id",
            "agent",
            "fixture",
            "expected",
            "oracle_kind",
            "exact_fields",
        ):
            if required not in item:
                raise OracleError(f"case[{index}] missing required key: {required}")
        case_id = item["id"]
        agent = item["agent"]
        if not isinstance(case_id, str) or not case_id:
            raise OracleError(f"case[{index}].id must be a non-empty string")
        if case_id in ids:
            raise OracleError(f"duplicate case id: {case_id}")
        if agent not in SUPPORTED_AGENTS:
            raise OracleError(f"case {case_id}: unsupported agent {agent!r}")
        fixture = repo_path(item["fixture"])
        expected = repo_path(item["expected"])
        if not fixture.exists():
            raise OracleError(f"case {case_id}: fixture does not exist: {fixture}")
        if not expected.is_file():
            raise OracleError(
                f"case {case_id}: expected file does not exist: {expected}"
            )
        oracle_kind = item["oracle_kind"]
        oracle_command = item.get("oracle_command")
        if agent in DONOR_AGENTS:
            if oracle_kind != "transcript_builder":
                raise OracleError(
                    f"case {case_id}: donor-supported agent must use transcript_builder"
                )
            if not isinstance(oracle_command, str) or not oracle_command:
                raise OracleError(f"case {case_id}: donor oracle command is required")
            required_fragment = f"--agent {agent} --l1-only"
            if (
                required_fragment not in oracle_command
                or "build-session-record" not in oracle_command
            ):
                raise OracleError(
                    f"case {case_id}: oracle command must be exact build-session-record template for {agent}"
                )
        elif oracle_kind != "rust_golden" or oracle_command is not None:
            raise OracleError(
                "Junie must use rust_golden and must not declare a donor command"
            )

        exact_fields = item["exact_fields"]
        if (
            not isinstance(exact_fields, list)
            or not exact_fields
            or not all(isinstance(value, str) and value for value in exact_fields)
        ):
            raise OracleError(
                f"case {case_id}: exact_fields must be a non-empty string list"
            )
        assertions = item.get("heuristic_assertions", [])
        if not isinstance(assertions, list):
            raise OracleError(
                f"case {case_id}: heuristic_assertions must be an array of tables"
            )
        for assertion in assertions:
            validate_assertion_shape(case_id, assertion)

        cases.append(
            Case(
                id=case_id,
                agent=agent,
                fixture=fixture,
                expected=expected,
                oracle_kind=oracle_kind,
                oracle_command=oracle_command,
                exact_fields=tuple(exact_fields),
                heuristic_assertions=tuple(assertions),
            )
        )
        ids.add(case_id)
        agents.add(agent)

    if agents != SUPPORTED_AGENTS:
        missing = sorted(SUPPORTED_AGENTS - agents)
        raise OracleError(
            f"manifest must cover every agent; missing: {', '.join(missing)}"
        )

    validate_contract(REPO_ROOT / "docs/PARSER_ENGINE_CONTRACT.md")
    matrix_raw = data.get("normative_matrix")
    if not isinstance(matrix_raw, str) or not matrix_raw:
        raise OracleError("manifest requires normative_matrix path")
    matrix_path = repo_path(matrix_raw)
    if matrix_path.exists():
        validate_normative_matrix(matrix_path)
    elif require_normative_matrix:
        raise OracleError(f"C0A normative matrix is not available: {matrix_path}")
    return cases


def validate_assertion_shape(case_id: str, assertion: Any) -> None:
    if not isinstance(assertion, dict):
        raise OracleError(f"case {case_id}: heuristic assertion must be a table")
    path = assertion.get("path")
    op = assertion.get("op")
    if not isinstance(path, str) or not path:
        raise OracleError(f"case {case_id}: heuristic assertion requires path")
    if op not in {"nonempty", "contains_any", "equals"}:
        raise OracleError(f"case {case_id}: unsupported heuristic op {op!r}")
    if op == "contains_any":
        values = assertion.get("values")
        if (
            not isinstance(values, list)
            or not values
            or not all(isinstance(v, str) for v in values)
        ):
            raise OracleError(f"case {case_id}: contains_any requires string values")
    if op == "equals" and "value" not in assertion:
        raise OracleError(f"case {case_id}: equals requires value")


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise OracleError(f"cannot read JSON {path}: {exc}") from exc


def field_value(document: Any, path: str) -> Any:
    value = document
    traversed = "$"
    for part in path.split("."):
        traversed += f".{part}"
        if not isinstance(value, dict) or part not in value:
            raise OracleError(f"missing field {traversed}")
        value = value[part]
    return value


def normalize_document(document: dict[str, Any]) -> dict[str, Any]:
    if document.get("schema") == "parser_oracle.envelope.v1":
        return document
    if document.get("schema") != "session_record.v1":
        raise OracleError(
            "actual JSON must be parser_oracle.envelope.v1 or session_record.v1"
        )

    provenance = document.get("provenance") or {}
    chat = document.get("chat") or {}
    visible_turns: list[dict[str, Any]] = []
    for turn in chat.get("turns") or []:
        if turn.get("role") not in {"user", "assistant"}:
            continue
        if turn.get("kind") not in {"message", "dispatched_workflow_prompt"}:
            continue
        visible_turns.append(
            {
                "ordinal": len(visible_turns),
                "role": turn.get("role"),
                "kind": "message",
                "text": turn.get("text_preview", ""),
            }
        )
    coverage = document.get("parser_coverage") or {}
    parse_status = coverage.get("parse_status")
    visible_status = (
        "fatal"
        if parse_status == "failed"
        else ("partial_visible" if parse_status == "partial" else "complete_visible")
    )
    boundaries: list[str] = []
    for segment in document.get("segments") or []:
        for boundary in segment.get("boundary") or []:
            kind = boundary.get("kind")
            if (
                kind == "encrypted_payload"
                and "opaque_reasoning_present" not in boundaries
            ):
                boundaries.append("opaque_reasoning_present")
            if kind in {"unknown_payload_type", "malformed_json", "oversized_line"}:
                if "unsupported_visible_event" not in boundaries:
                    boundaries.append("unsupported_visible_event")
    intent = document.get("intent") or {}
    return {
        "schema": "parser_oracle.envelope.v1",
        "agent": provenance.get("agent"),
        "session_id": document.get("session_id") or provenance.get("session_id"),
        "visible_turns": visible_turns,
        "coverage": {
            "raw_units": coverage.get("raw_line_count"),
            "consumed": coverage.get("consumed_count"),
            "skipped": coverage.get("skipped_count"),
        },
        "status": {"visible": visible_status, "boundaries": boundaries},
        "usage": document.get("usage", []),
        "heuristic": {
            "intent_summary": intent.get("body") or intent.get("summary") or ""
        },
    }


def compare_case(case: Case, actual_document: dict[str, Any]) -> None:
    expected = normalize_document(load_json(case.expected))
    actual = normalize_document(actual_document)
    for path in case.exact_fields:
        expected_value = field_value(expected, path)
        actual_value = field_value(actual, path)
        if expected_value != actual_value:
            raise OracleError(
                f"exact mismatch at $.{path}: expected {expected_value!r}, got {actual_value!r}"
            )
    for assertion in case.heuristic_assertions:
        actual_value = field_value(actual, assertion["path"])
        op = assertion["op"]
        if op == "nonempty" and not actual_value:
            raise OracleError(
                f"heuristic assertion failed at $.{assertion['path']}: expected nonempty"
            )
        if op == "contains_any":
            text = str(actual_value).casefold()
            if not any(value.casefold() in text for value in assertion["values"]):
                raise OracleError(
                    f"heuristic assertion failed at $.{assertion['path']}: contains_any {assertion['values']!r}"
                )
        if op == "equals" and actual_value != assertion["value"]:
            raise OracleError(
                f"heuristic assertion failed at $.{assertion['path']}: expected {assertion['value']!r}"
            )


def self_test() -> None:
    manifest_path = REPO_ROOT / "tests/parser_oracle/manifest.toml"
    cases = parse_manifest(manifest_path)
    codex = next(case for case in cases if case.id == "codex_minimal")
    expected = load_json(codex.expected)
    compare_case(codex, expected)

    heuristic_variant = copy.deepcopy(expected)
    heuristic_variant["heuristic"]["intent_summary"] = "Harness oracle implementation"
    compare_case(codex, heuristic_variant)

    corrupted = copy.deepcopy(expected)
    corrupted["session_id"] = "corrupted"
    try:
        compare_case(codex, corrupted)
    except OracleError as exc:
        if "$.session_id" not in str(exc):
            raise OracleError(f"corruption failure omitted field path: {exc}") from exc
    else:
        raise OracleError("intentionally corrupted golden unexpectedly passed")

    broken_heuristic = copy.deepcopy(expected)
    broken_heuristic["heuristic"]["intent_summary"] = ""
    try:
        compare_case(codex, broken_heuristic)
    except OracleError as exc:
        if "$.heuristic.intent_summary" not in str(exc):
            raise OracleError(f"heuristic failure omitted field path: {exc}") from exc
    else:
        raise OracleError("broken heuristic assertion unexpectedly passed")

    with tempfile.TemporaryDirectory(prefix="aicx-oracle-selftest-") as tmp:
        tmp_path = Path(tmp)
        matrix = tmp_path / "fields.toml"
        matrix.write_text(
            '[[field]]\npath = "session_id"\nclassification = "normative"\n',
            encoding="utf-8",
        )
        validate_normative_matrix(matrix)
        bad_manifest = tmp_path / "bad.toml"
        bad_manifest.write_text(
            'schema_version = 1\n[[case]]\nid = "broken"\n', encoding="utf-8"
        )
        try:
            parse_manifest(bad_manifest)
        except OracleError as exc:
            if "missing required key: agent" not in str(exc):
                raise OracleError(
                    f"missing-section test failed unclearly: {exc}"
                ) from exc
        else:
            raise OracleError(
                "manifest missing agent/fixture/expected unexpectedly passed"
            )
    print(
        "parser oracle self-test: PASS (exact path + heuristic predicates + manifest contract)"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest", type=Path, default=REPO_ROOT / "tests/parser_oracle/manifest.toml"
    )
    parser.add_argument("--case", dest="case_id")
    parser.add_argument("--actual", type=Path)
    parser.add_argument("--require-normative-matrix", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test()
            return 0
        cases = parse_manifest(
            args.manifest.resolve(),
            require_normative_matrix=args.require_normative_matrix,
        )
        if args.case_id is None and args.actual is None:
            print(f"parser oracle manifest: PASS ({len(cases)} cases)")
            return 0
        if args.case_id is None or args.actual is None:
            raise OracleError("--case and --actual must be provided together")
        case = next((item for item in cases if item.id == args.case_id), None)
        if case is None:
            raise OracleError(f"unknown case id: {args.case_id}")
        actual = load_json(args.actual.resolve())
        if not isinstance(actual, dict):
            raise OracleError("actual JSON root must be an object")
        compare_case(case, actual)
        print(f"parser oracle comparison: PASS ({case.id})")
        return 0
    except OracleError as exc:
        print(f"parser oracle comparison: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
