# human_shape_260528

Bounded projection of Junie session `session-260528-231721-1ksa`.

## Source (not in repo)

`~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/junie/session-260528-231721-1ksa.events.jsonl`

Raw 1.4 MB / 1197 lines, all `kind=SessionA2uxEvent`. There is no `UserPromptEvent` in this log. This projection keeps selected `event.agentEvent.kind` values (255 lines, 70015 bytes). Production extract heading counts match the full source (0 user, 1 empty assistant).

## Derivation (deterministic)

```bash
SRC=~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/junie/session-260528-231721-1ksa.events.jsonl
DST=tests/fixtures/parser_engine/junie/human_shape_260528.jsonl
python3 -c '
import json, sys
wanted = {
    "CurrentDirectoryUpdatedEvent",
    "AgentThoughtBlockUpdatedEvent",
    "SuggestPlanEvent",
    "ResultBlockUpdatedEvent",
    "AgentTaskNameUpdatedEvent",
    "LlmResponseMetadataEvent",
}
firsts = {"AgentCurrentStatusUpdatedEvent", "EnvironmentVariablesUpdatedEvent"}
seen = set()
src, dst = sys.argv[1], sys.argv[2]
with open(src) as f, open(dst, "w") as g:
    for line in f:
        k = ((json.loads(line).get("event") or {}).get("agentEvent") or {}).get("kind")
        take = k in wanted
        if k in firsts and k not in seen:
            take = True
            seen.add(k)
        if take:
            g.write(line)
' "$SRC" "$DST"
```

## Extract (baseline aicx@a687754)

```bash
aicx extract junie --file tests/fixtures/parser_engine/junie/human_shape_260528.jsonl --conversation --user-only -o /tmp/260528-user.md
```
