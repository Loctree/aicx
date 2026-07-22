#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: tools/bench_dense_migration.sh --verify-only
       tools/bench_dense_migration.sh [--rows N] [--dim N] [--queries N] [--top-k N] [--output PATH] [--keep]

Build an isolated AICX_HOME-shaped dense-index corpus, materialize the mmap
replacement, and compare it against the legacy duplicate NDJSON pair without
touching the live ~/.aicx store.
USAGE
  exit 2
}

rows=4096
dim=128
queries=24
top_k=10
verify_only=false
keep=false
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify-only)
      verify_only=true
      rows=768
      dim=256
      queries=16
      shift
      ;;
    --rows)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      rows=$2
      shift 2
      ;;
    --dim)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      dim=$2
      shift 2
      ;;
    --queries)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      queries=$2
      shift 2
      ;;
    --top-k)
      [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || usage
      top_k=$2
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || usage
      output=$2
      shift 2
      ;;
    --keep)
      keep=true
      shift
      ;;
    -h|--help)
      usage
      ;;
    *)
      usage
      ;;
  esac
done

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

python3 - "$repo_root" "$rows" "$dim" "$queries" "$top_k" "$verify_only" "$keep" "$output" <<'PY'
from __future__ import annotations

import gc
import hashlib
import heapq
import json
import math
import mmap
import os
import shutil
import statistics
import struct
import sys
import tempfile
import time
import tracemalloc
from pathlib import Path


repo_root = Path(sys.argv[1])
rows = int(sys.argv[2])
dim = int(sys.argv[3])
query_count = int(sys.argv[4])
top_k = int(sys.argv[5])
verify_only = sys.argv[6] == "true"
keep = sys.argv[7] == "true"
output_arg = sys.argv[8]

projects = [
    "Loctree/aicx",       # exact live identity shape
    "loctree/AICX",       # case-drift twin
    "aicx",               # bare repo identity
    "loctree_aicx",       # underscore project-scoped bucket from the live baseline
    "Loctree/loctree",    # adjacent owner namespace
    "non-repository-contexts",
]
project_filter = "loctree_aicx"
agents = ["codex", "claude", "junie", "gemini"]
kinds = ["conversations", "reports", "plans"]

baseline = {
    "unscoped": {"bucket": "_all", "legacy_pair_bytes": 15_000_000_000 * 2, "max_rss_bytes": 5_500_000_000, "wall_seconds": 71.8, "user_cpu_seconds": 64.0},
    "project_scoped": {"bucket": "loctree_aicx", "legacy_dense_bytes": 221_000_000, "max_rss_bytes": 285_000_000, "wall_seconds": 5.1},
}
budgets = {
    "dense_payload_max_ratio_of_legacy_duplicate_pair": 0.60,
    "peak_rss_max_bytes": int(1.25 * 1024 * 1024 * 1024),
    "warm_project_p95_max_seconds": 2.0,
    "warm_global_p95_max_seconds": 8.0,
    "exact_top_k_parity": 1.0,
}


def now_ms() -> int:
    return int(time.time() * 1000)


def vector_for(i: int) -> list[float]:
    # Deterministic non-zero unit-ish vector. The formula is cheap, stable, and
    # produces enough separation that an exact row query has identical top-k in
    # the legacy and mmap paths.
    values = [math.sin((i + 1) * (j + 3) * 0.017) + math.cos((i + 11) * (j + 5) * 0.011) for j in range(dim)]
    norm = math.sqrt(sum(v * v for v in values)) or 1.0
    return [float(v / norm) for v in values]


def row_meta(i: int) -> dict[str, object]:
    project = projects[i % len(projects)]
    return {
        "id": f"bench-{i:07d}",
        "project": project,
        "agent": agents[i % len(agents)],
        "date": "20260722",
        "path": str(Path("store") / project.replace("/", "_") / f"bench-{i:07d}.md"),
        "kind": kinds[i % len(kinds)],
        "session_id": f"session-{i // len(projects):07d}",
        "frame_kind": "agent_reply" if i % 5 else "user_msg",
        "cwd": str(repo_root),
    }


def write_legacy_index(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    header = {
        "schema_version": "1.0",
        "model_id": "w4-bench-deterministic",
        "model_profile": "benchmark",
        "dimension": dim,
        "generated_at": "2026-07-22T00:00:00Z",
        "entry_count": rows,
    }
    with path.open("w", encoding="utf-8") as fh:
        fh.write(json.dumps(header, separators=(",", ":")) + "\n")
        for i in range(rows):
            meta = row_meta(i)
            entry = dict(meta)
            entry["embedding"] = vector_for(i)
            fh.write(json.dumps(entry, separators=(",", ":")) + "\n")


def source_hash(path: Path) -> bytes:
    digest = hashlib.blake2b(digest_size=32)
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(block)
    return digest.digest()


def write_mmap_payload(path: Path, committed_index: Path) -> dict[str, int]:
    path.parent.mkdir(parents=True, exist_ok=True)
    metadata_blobs: list[bytes] = []
    metadata_len = 0
    for i in range(rows):
        meta = row_meta(i)
        blob = json.dumps(
            {
                "chunk_id": meta["id"],
                "source_path": meta["path"],
                "metadata": {"project": meta["project"], "agent": meta["agent"], "kind": meta["kind"]},
            },
            separators=(",", ":"),
        ).encode("utf-8")
        metadata_blobs.append(blob)
        metadata_len += len(blob)

    header_len = 128
    refs_len = rows * 16
    vectors_len = rows * dim * 4
    refs_offset = header_len
    vectors_offset = refs_offset + refs_len
    metadata_offset = vectors_offset + vectors_len
    file_len = metadata_offset + metadata_len
    header = bytearray(header_len)
    header[0:8] = b"AICXDMM1"
    struct.pack_into("<H", header, 8, 1)
    struct.pack_into("<I", header, 10, 0x01020304)
    struct.pack_into("<H", header, 14, header_len)
    struct.pack_into("<I", header, 16, dim)
    header[20] = 1  # cosine
    struct.pack_into("<Q", header, 24, rows)
    header[32:64] = source_hash(committed_index)
    for offset, value in (
        (64, refs_offset),
        (72, refs_len),
        (80, vectors_offset),
        (88, vectors_len),
        (96, metadata_offset),
        (104, metadata_len),
        (112, file_len),
    ):
        struct.pack_into("<Q", header, offset, value)

    tmp = path.with_name(path.name + ".tmp")
    with tmp.open("wb") as fh:
        fh.write(header)
        cursor = 0
        for blob in metadata_blobs:
            fh.write(struct.pack("<QII", cursor, len(blob), 0))
            cursor += len(blob)
        for i in range(rows):
            fh.write(struct.pack(f"<{dim}f", *vector_for(i)))
        for blob in metadata_blobs:
            fh.write(blob)
        fh.flush()
        os.fsync(fh.fileno())
    tmp.replace(path)
    return {"refs_len": refs_len, "vectors_len": vectors_len, "metadata_len": metadata_len, "file_len": file_len}


def publish_generation(hybrid_root: Path, committed_index: Path, generation: str) -> dict[str, object]:
    generation_dir = hybrid_root / "generations" / generation
    dense_path = generation_dir / "dense.exact_mmap_v1.bin"
    payload = write_mmap_payload(dense_path, committed_index)
    manifest = {
        "schema": "aicx.hybrid.manifest.v1",
        "generation_id": generation,
        "source_chunk_count": rows,
        "dense_kind": "exact_mmap_v1",
        "dense_count": rows,
        "dense_payload": "dense.exact_mmap_v1.bin",
        "dense_payload_schema": "aicx.dense.exact_mmap.v1",
        "distance": "cosine",
        "dimension": dim,
        "lexical_doc_count": rows,
    }
    (generation_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True), encoding="utf-8")
    pointer_tmp = hybrid_root / "CURRENT.tmp"
    pointer_tmp.write_text(generation + "\n", encoding="utf-8")
    pointer_tmp.replace(hybrid_root / "CURRENT")
    return {"generation_dir": str(generation_dir), "dense_path": str(dense_path), "payload": payload, "manifest": manifest}


def simulate_failed_publish(hybrid_root: Path, committed_index: Path) -> dict[str, object]:
    before = (hybrid_root / "CURRENT").read_text(encoding="utf-8").strip()
    corrupt_dir = hybrid_root / "generations" / "gen-corrupt-copy"
    corrupt_dir.mkdir(parents=True, exist_ok=True)
    full_payload = corrupt_dir / "dense.exact_mmap_v1.bin.full"
    write_mmap_payload(full_payload, committed_index)
    data = full_payload.read_bytes()
    (corrupt_dir / "dense.exact_mmap_v1.bin").write_bytes(data[: max(1, len(data) // 3)])
    full_payload.unlink()
    after_corrupt = (hybrid_root / "CURRENT").read_text(encoding="utf-8").strip()

    interrupted_dir = hybrid_root / "generations" / "gen-interrupted-copy"
    interrupted_dir.mkdir(parents=True, exist_ok=True)
    write_mmap_payload(interrupted_dir / "dense.exact_mmap_v1.bin", committed_index)
    # No manifest and no pointer flip: this models a killed copy before publish.
    after_interrupt = (hybrid_root / "CURRENT").read_text(encoding="utf-8").strip()
    return {
        "current_before": before,
        "current_after_corrupt_copy": after_corrupt,
        "current_after_interrupted_copy": after_interrupt,
        "current_generation_untouched": before == after_corrupt == after_interrupt,
        "corrupt_generation_dir": str(corrupt_dir),
        "interrupted_generation_dir": str(interrupted_dir),
    }


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, math.ceil((pct / 100.0) * len(ordered)) - 1)
    return ordered[idx]


def score(query: list[float], row: list[float]) -> float:
    return sum(a * b for a, b in zip(query, row))


def selected_query_rows() -> list[int]:
    if query_count <= 1:
        return [0]
    step = max(1, rows // query_count)
    selected = [(idx * step + idx * 17) % rows for idx in range(query_count)]
    # Ensure project-filter coverage.
    for idx in range(0, len(selected), 2):
        selected[idx] = (idx * step // 2) * len(projects) + projects.index(project_filter)
        selected[idx] %= rows
    return selected


def top_ids_from_heap(heap: list[tuple[float, str]]) -> list[str]:
    return [chunk_id for score_value, chunk_id in sorted(heap, key=lambda item: (-item[0], item[1]))]


def run_legacy(index_path: Path, query_rows: list[int], scoped: bool) -> dict[str, object]:
    tracemalloc.start()
    start = time.perf_counter()
    entries = []
    with index_path.open("r", encoding="utf-8") as fh:
        next(fh)
        for line in fh:
            row = json.loads(line)
            if scoped and row.get("project") != project_filter:
                continue
            entries.append((row["id"], row["embedding"]))
    startup_seconds = time.perf_counter() - start
    latencies: list[float] = []
    results: dict[str, list[str]] = {}
    for row_id in query_rows:
        query = vector_for(row_id)
        qid = f"q{row_id:07d}"
        started = time.perf_counter()
        heap: list[tuple[float, str]] = []
        for chunk_id, embedding in entries:
            candidate = (score(query, embedding), chunk_id)
            if len(heap) < top_k:
                heapq.heappush(heap, candidate)
            elif candidate > heap[0]:
                heapq.heapreplace(heap, candidate)
        latencies.append(time.perf_counter() - started)
        results[qid] = top_ids_from_heap(heap)
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    loaded = len(entries)
    del entries
    gc.collect()
    return {
        "loaded_rows": loaded,
        "startup_seconds": startup_seconds,
        "p50_seconds": statistics.median(latencies) if latencies else 0.0,
        "p95_seconds": percentile(latencies, 95),
        "heap_peak_bytes": peak,
        "top_k": results,
    }


def read_header(mm: mmap.mmap) -> dict[str, int]:
    if mm[0:8] != b"AICXDMM1":
        raise RuntimeError("mmap dense magic mismatch")
    return {
        "dim": struct.unpack_from("<I", mm, 16)[0],
        "count": struct.unpack_from("<Q", mm, 24)[0],
        "refs_offset": struct.unpack_from("<Q", mm, 64)[0],
        "vectors_offset": struct.unpack_from("<Q", mm, 80)[0],
        "metadata_offset": struct.unpack_from("<Q", mm, 96)[0],
    }


def extract_ascii_string(mm: mmap.mmap, start: int, end: int, key: bytes) -> str:
    field = b'"' + key + b'":"'
    field_start = mm.find(field, start, end)
    if field_start < 0:
        raise RuntimeError(f"missing mmap metadata field {key.decode('ascii')}")
    value_start = field_start + len(field)
    value_end = mm.find(b'"', value_start, end)
    if value_end < 0:
        raise RuntimeError(f"unterminated mmap metadata field {key.decode('ascii')}")
    return mm[value_start:value_end].decode("utf-8")


def read_project_and_id(mm: mmap.mmap, header: dict[str, int], row: int) -> tuple[str, str]:
    ref_offset = header["refs_offset"] + row * 16
    metadata_rel, metadata_len, _ = struct.unpack_from("<QII", mm, ref_offset)
    blob_start = header["metadata_offset"] + metadata_rel
    blob_end = blob_start + metadata_len
    return (
        extract_ascii_string(mm, blob_start, blob_end, b"project"),
        extract_ascii_string(mm, blob_start, blob_end, b"chunk_id"),
    )


def run_mmap(dense_path: Path, query_rows: list[int], scoped: bool) -> dict[str, object]:
    tracemalloc.start()
    start = time.perf_counter()
    with dense_path.open("rb") as fh:
        mm = mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ)
        header = read_header(mm)
        startup_seconds = time.perf_counter() - start
        latencies: list[float] = []
        results: dict[str, list[str]] = {}
        row_format = f"<{header['dim']}f"
        row_size = header["dim"] * 4
        for row_id in query_rows:
            query = vector_for(row_id)
            qid = f"q{row_id:07d}"
            started = time.perf_counter()
            heap: list[tuple[float, str]] = []
            for row in range(header["count"]):
                project, chunk_id = read_project_and_id(mm, header, row)
                if scoped and project != project_filter:
                    continue
                vector_offset = header["vectors_offset"] + row * row_size
                embedding = struct.unpack_from(row_format, mm, vector_offset)
                candidate = (sum(a * b for a, b in zip(query, embedding)), chunk_id)
                if len(heap) < top_k:
                    heapq.heappush(heap, candidate)
                elif candidate > heap[0]:
                    heapq.heapreplace(heap, candidate)
            latencies.append(time.perf_counter() - started)
            results[qid] = top_ids_from_heap(heap)
        mm.close()
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    return {
        "startup_seconds": startup_seconds,
        "p50_seconds": statistics.median(latencies) if latencies else 0.0,
        "p95_seconds": percentile(latencies, 95),
        "heap_peak_bytes": peak,
        "top_k": results,
    }


def parity(left: dict[str, list[str]], right: dict[str, list[str]]) -> dict[str, object]:
    keys = sorted(set(left) | set(right))
    matches = [key for key in keys if left.get(key) == right.get(key)]
    mismatches = [key for key in keys if left.get(key) != right.get(key)]
    return {
        "queries": len(keys),
        "matched": len(matches),
        "ratio": 1.0 if not keys else len(matches) / len(keys),
        "mismatches": mismatches[:10],
    }


work = Path(tempfile.mkdtemp(prefix="aicx-dense-migration-bench-"))
try:
    home = work / "AICX_HOME"
    committed = home / "indexed" / "_all" / "embeddings.ndjson"
    legacy_twin = home / "indexed" / "_all" / "hybrid" / "dense_brute_force.ndjson"
    hybrid_root = home / "indexed" / "_all" / "hybrid"

    t0 = time.perf_counter()
    write_legacy_index(committed)
    legacy_twin.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(committed, legacy_twin)
    publish_generation(hybrid_root, committed, "gen-current")
    failed_publish = simulate_failed_publish(hybrid_root, committed)
    published = publish_generation(hybrid_root, committed, "gen-mmap-benchmark")
    build_seconds = time.perf_counter() - t0

    dense_path = Path(published["dense_path"])
    query_rows = selected_query_rows()
    reverse_query_rows = list(reversed(query_rows))

    legacy_global = run_legacy(committed, query_rows, scoped=False)
    legacy_project = run_legacy(committed, query_rows, scoped=True)
    mmap_global_cold = run_mmap(dense_path, query_rows, scoped=False)
    mmap_project_cold = run_mmap(dense_path, query_rows, scoped=True)
    mmap_global_warm = run_mmap(dense_path, query_rows, scoped=False)
    mmap_project_warm = run_mmap(dense_path, query_rows, scoped=True)
    mmap_global_reverse = run_mmap(dense_path, reverse_query_rows, scoped=False)

    legacy_pair_bytes = committed.stat().st_size + legacy_twin.stat().st_size
    dense_payload_bytes = dense_path.stat().st_size
    disk_ratio = dense_payload_bytes / legacy_pair_bytes if legacy_pair_bytes else 1.0
    global_parity = parity(legacy_global["top_k"], mmap_global_warm["top_k"])
    project_parity = parity(legacy_project["top_k"], mmap_project_warm["top_k"])
    reverse_parity = parity(mmap_global_warm["top_k"], mmap_global_reverse["top_k"])

    status_checks = {
        "disk_budget_met": disk_ratio <= budgets["dense_payload_max_ratio_of_legacy_duplicate_pair"],
        "global_p95_budget_met": mmap_global_warm["p95_seconds"] <= budgets["warm_global_p95_max_seconds"],
        "project_p95_budget_met": mmap_project_warm["p95_seconds"] <= budgets["warm_project_p95_max_seconds"],
        "global_top_k_parity_met": global_parity["ratio"] == 1.0,
        "project_top_k_parity_met": project_parity["ratio"] == 1.0,
        "reverse_order_parity_met": reverse_parity["ratio"] == 1.0,
        "current_generation_untouched_by_failed_copies": failed_publish["current_generation_untouched"],
        "mmap_heap_not_payload_sized": mmap_global_warm["heap_peak_bytes"] < dense_payload_bytes * 0.25,
    }
    production_scale = rows >= 300_000 and dim >= 4096
    outcome = "budgets_met" if all(status_checks.values()) else "budgets_missed"
    if verify_only:
        outcome = "verify_only_pass" if all(status_checks.values()) else "verify_only_fail"

    report = {
        "schema": "aicx.dense_migration_benchmark.v1",
        "mode": "verify_only" if verify_only else "benchmark",
        "production_scale": production_scale,
        "isolated_aicx_home": str(home),
        "live_store_mutated": False,
        "rows": rows,
        "dim": dim,
        "queries": len(query_rows),
        "top_k": top_k,
        "taxonomy": {"projects": projects, "project_filter": project_filter},
        "baseline_live_dispatcher": baseline,
        "budgets": budgets,
        "build_seconds": build_seconds,
        "disk": {
            "legacy_embeddings_bytes": committed.stat().st_size,
            "legacy_twin_bytes": legacy_twin.stat().st_size,
            "legacy_duplicate_pair_bytes": legacy_pair_bytes,
            "mmap_dense_payload_bytes": dense_payload_bytes,
            "mmap_to_legacy_duplicate_pair_ratio": disk_ratio,
        },
        "red_proof": failed_publish,
        "green_proof": {
            "published_generation": published["manifest"]["generation_id"],
            "current_generation": (hybrid_root / "CURRENT").read_text(encoding="utf-8").strip(),
            "dense_payload_schema": published["manifest"]["dense_payload_schema"],
        },
        "latency": {
            "legacy_global": {key: legacy_global[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes", "loaded_rows")},
            "legacy_project": {key: legacy_project[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes", "loaded_rows")},
            "mmap_global_cold": {key: mmap_global_cold[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes")},
            "mmap_project_cold": {key: mmap_project_cold[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes")},
            "mmap_global_warm": {key: mmap_global_warm[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes")},
            "mmap_project_warm": {key: mmap_project_warm[key] for key in ("startup_seconds", "p50_seconds", "p95_seconds", "heap_peak_bytes")},
        },
        "parity": {"global": global_parity, "project": project_parity, "reverse_query_order": reverse_parity},
        "status_checks": status_checks,
        "outcome": outcome,
        "recommendation": "mmap_stands" if outcome in ("budgets_met", "verify_only_pass") else "trigger_memex_search_transplant_brief",
        "limitations": [] if production_scale else ["This run is a deterministic contract/verify corpus, not the observed ~300k x 4096 production-scale falsification."],
    }

    rendered = json.dumps(report, indent=2, sort_keys=True)
    if output_arg:
        output = Path(output_arg)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered + "\n", encoding="utf-8")
    print(rendered)
    if outcome.endswith("fail") or outcome == "budgets_missed":
        raise SystemExit(1)
finally:
    if not keep:
        shutil.rmtree(work, ignore_errors=True)
PY