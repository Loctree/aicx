# human_shape_01a042f9_interagent

Bounded projection of Codex rollout `01a042f9` for Amendment A2 (inter-agent lane).

Parked next to `ASSERTIONS.md`, not under `tests/fixtures/parser_engine/codex/` — that directory is W0-T1.

## Source (34.7 MB, not in repo)

`/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/codex/diagnostic-01a042f9/rollout-2026-08-27T13-27-04-01a042f9-3803-7772-bf00-1887a50aaf89.jsonl`

This file keeps, in source order: `session_meta`, every `response_item.payload.type == "agent_message"` (28), every `inter_agent_communication_metadata` (28), and the first `type == "compacted"` (one window with `replacement_history` length 8). Size 188439 bytes.

Raw `agent_message` count 28 is an assertion on the **external** source, not on this slice.

## Derivation (deterministic)

```bash
SRC=/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/codex/diagnostic-01a042f9/rollout-2026-08-27T13-27-04-01a042f9-3803-7772-bf00-1887a50aaf89.jsonl
DST=tests/fixtures/parser_engine/human_shape_01a042f9_interagent.jsonl
python3 -c '
import json, sys
src, dst = sys.argv[1], sys.argv[2]
kept_compacted = False
with open(src) as f, open(dst, "w") as g:
    for line in f:
        o = json.loads(line)
        t = o.get("type")
        pl = o.get("payload") if isinstance(o.get("payload"), dict) else {}
        pt = pl.get("type")
        keep = False
        if t in ("session_meta", "inter_agent_communication_metadata"):
            keep = True
        elif t == "response_item" and pt == "agent_message":
            keep = True
        elif t == "compacted" and not kept_compacted:
            keep = True
            kept_compacted = True
        if keep:
            g.write(line)
' "$SRC" "$DST"
```

## Extract (baseline aicx@a687754)

```bash
aicx extract codex --file tests/fixtures/parser_engine/human_shape_01a042f9_interagent.jsonl --conversation -o /tmp/01a042f9.md
```

Today: 28 assistant headings, each with `Message Type` / `Task name` / `Sender` envelope. `--kind inter_agent` is not a flag (`unexpected argument '--kind'`, exit 2). Compacted-only extract of `session_meta`+first `compacted` yields 0 headings.
