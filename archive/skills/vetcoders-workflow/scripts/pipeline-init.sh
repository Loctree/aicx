#!/usr/bin/env bash
# pipeline-init.sh — Initialize ERi pipeline directory structure
# Usage: bash pipeline-init.sh <slug> [root-dir]
# Created by M&K (c)2026 VetCoders

set -euo pipefail

SLUG="${1:?Usage: pipeline-init.sh <slug> [root-dir]}"
ROOT="${2:-$(pwd)}"
PIPELINE_DIR="$ROOT/.ai-agents/pipeline/$SLUG"

if [ -d "$PIPELINE_DIR" ]; then
    echo "Pipeline '$SLUG' already exists at $PIPELINE_DIR"
    echo "Contents:"
    ls -la "$PIPELINE_DIR"
    exit 1
fi

mkdir -p "$PIPELINE_DIR"/{plans,reports}

# Create CONTEXT.md skeleton
cat > "$PIPELINE_DIR/CONTEXT.md" << 'CONTEXT_EOF'
# Examination: SLUG_PLACEHOLDER

Date: DATE_PLACEHOLDER
Pipeline: .ai-agents/pipeline/SLUG_PLACEHOLDER/

## Repo Health
- <!-- fill from repo-view -->

## Task Understanding
- User request: <!-- original request -->
- Interpreted scope: <!-- what needs to change -->

## Target Modules
<!-- fill from focus() -->

## Critical Files (slice results)
| File | LOC | Dependencies | Consumers | Risk |
|------|-----|-------------|-----------|------|

## Existing Symbols
<!-- fill from find() -->

## Risk Map
| File | Blast Radius | Mitigation |
|------|-------------|------------|

## Open Questions (for Research phase)
1.

## Phase Decision
- [ ] Research needed
- [ ] Skip to Implement
CONTEXT_EOF

# Replace placeholders (BSD sed — macOS only)
sed -i '' "s/SLUG_PLACEHOLDER/$SLUG/g" "$PIPELINE_DIR/CONTEXT.md"
sed -i '' "s/DATE_PLACEHOLDER/$(date +%Y-%m-%d)/g" "$PIPELINE_DIR/CONTEXT.md"

echo "Pipeline initialized: $PIPELINE_DIR"
echo ""
echo "Structure:"
echo "  $PIPELINE_DIR/"
echo "  ├── CONTEXT.md      (Phase 1: Examination output)"
echo "  ├── plans/           (Phase 3: Agent plans)"
echo "  └── reports/         (Phase 3: Agent reports)"
echo ""
echo "Next: Run Phase 1 (Examine) with loctree MCP tools"
echo "  RESEARCH.md will be created in Phase 2 if needed"
