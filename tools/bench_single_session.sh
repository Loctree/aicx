#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: tools/bench_single_session.sh --baseline-only <session-path>" >&2
  echo "       tools/bench_single_session.sh --engine-only --hard-threshold-ms <ms> <session-path>" >&2
  exit 2
}

if [[ $# -eq 2 && "$1" == "--baseline-only" ]]; then
  mode=baseline
  session_path=$2
elif [[ $# -eq 4 && "$1" == "--engine-only" && "$2" == "--hard-threshold-ms" ]]; then
  mode=engine
  threshold_ms=$3
  session_path=$4
  [[ "$threshold_ms" =~ ^[1-9][0-9]*$ ]] || usage
else
  usage
fi
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
donor_root=${TRANSCRIPT_BUILDER_ROOT:-/Volumes/LibraxisShare/vc-workspace/vetcoders/transcript-builder}

if [[ "$mode" == "engine" ]]; then
  [[ -f "$session_path" ]] || {
    echo "benchmark input is not a readable file: $session_path" >&2
    exit 1
  }
  output=$(
    cd "$repo_root"
    AICX_BENCH_SESSION="$session_path" \
      AICX_BENCH_THRESHOLD_MS="$threshold_ms" \
      cargo test --release -p aicx-parser --test adversarial_perf_helper \
        engine_only_benchmark -- --ignored --nocapture
  )
  json=$(printf '%s\n' "$output" | sed -n 's/^AICX_BENCH_JSON=//p' | tail -n 1)
  [[ -n "$json" ]] || {
    echo "engine benchmark emitted no JSON result" >&2
    exit 1
  }
  printf '%s\n' "$json"
  exit 0
fi

python3 - "$session_path" "$repo_root" "$donor_root" <<'PY'
from __future__ import annotations

import hashlib
import json
import math
import subprocess
import sys
import tempfile
import time
from pathlib import Path


source_arg = Path(sys.argv[1]).expanduser()
repo_root = Path(sys.argv[2])
donor_root = Path(sys.argv[3])
donor = donor_root / "transcript-builder"

if not source_arg.is_file():
    raise SystemExit(f"benchmark input is not a readable file: {source_arg}")
if not donor.is_file():
    raise SystemExit(f"Transcript Builder oracle executable is missing: {donor}")


def locate() -> tuple[Path, float, int]:
    started = time.perf_counter_ns()
    resolved = source_arg.resolve(strict=True)
    size = resolved.stat().st_size
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return resolved, elapsed_ms, size


def find_record(out_dir: Path) -> Path:
    records = sorted(out_dir.glob("*/session_record.json"))
    if len(records) != 1:
        raise RuntimeError(f"expected exactly one session_record.json, found {len(records)}")
    return records[0]


def project(record_path: Path) -> tuple[float, int, str]:
    started = time.perf_counter_ns()
    record = json.loads(record_path.read_text(encoding="utf-8"))
    coverage = record.get("parser_coverage") or {}
    projection = {
        "agent": (record.get("provenance") or {}).get("agent"),
        "session_id": record.get("session_id"),
        "turn_count": len(((record.get("chat") or {}).get("turns") or [])),
        "raw_units": coverage.get("raw_line_count"),
        "consumed": coverage.get("consumed_count"),
        "skipped": coverage.get("skipped_count"),
    }
    payload = json.dumps(projection, sort_keys=True, separators=(",", ":")).encode()
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return elapsed_ms, len(payload), hashlib.sha256(payload).hexdigest()


runs = []
for run_number in (1, 2):
    selected, locate_ms, selected_bytes = locate()
    with tempfile.TemporaryDirectory(prefix=f"aicx-parser-baseline-{run_number}-") as tmp:
        out_dir = Path(tmp) / "records"
        command = [
            str(donor),
            "build-session-record",
            str(selected),
            "--out-dir",
            str(out_dir),
            "--agent",
            "auto",
            "--l1-only",
        ]
        parse_started = time.perf_counter_ns()
        completed = subprocess.run(
            command,
            cwd=donor_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        parse_ms = (time.perf_counter_ns() - parse_started) / 1_000_000
        if completed.returncode != 0:
            message = completed.stderr.strip() or completed.stdout.strip()
            raise SystemExit(f"Transcript Builder baseline failed (run {run_number}): {message}")
        record_path = find_record(out_dir)
        projection_ms, projection_bytes, projection_sha256 = project(record_path)
        runs.append(
            {
                "run": run_number,
                "locate_ms": locate_ms,
                "parse_ms": parse_ms,
                "projection_ms": projection_ms,
                "total_ms": locate_ms + parse_ms + projection_ms,
                "opened_source_files": 1,
                "opened_source_bytes": selected_bytes,
                "projection_bytes": projection_bytes,
                "projection_sha256": projection_sha256,
            }
        )

result = {
    "schema": "aicx.parser_benchmark.v1",
    "mode": "baseline_only",
    "input": str(source_arg.resolve()),
    "input_copied_to_repo": False,
    "oracle": str(donor),
    "runs": runs,
}

required_numeric = (
    "locate_ms",
    "parse_ms",
    "projection_ms",
    "total_ms",
    "opened_source_files",
    "opened_source_bytes",
    "projection_bytes",
)
if len(runs) != 2:
    raise SystemExit("benchmark did not produce two consecutive runs")
for run in runs:
    for key in required_numeric:
        value = run.get(key)
        if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value):
            raise SystemExit(f"benchmark metric missing or non-finite: run={run.get('run')} metric={key}")
        if value < 0:
            raise SystemExit(f"benchmark metric is negative: run={run.get('run')} metric={key}")
    if run["parse_ms"] <= 0 or run["opened_source_files"] <= 0 or run["opened_source_bytes"] <= 0:
        raise SystemExit(f"benchmark required metric is zero: run={run['run']}")

print(json.dumps(result, sort_keys=True, separators=(",", ":")))
PY
