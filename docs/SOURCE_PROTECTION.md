# Source Protection, Privacy, And Team Sharing

AICX reads private agent-session roots, so the product contract is deliberately
conservative: default discovery is read-only. Normal `aicx list`, extractor,
store, search, and steering commands must not initialize `.git`, write hidden
source snapshots, configure remotes, or upload private user data.

Source protection exists for operators who explicitly want local repair history
for roots such as `$HOME/.aicx`, `$HOME/.codex/sessions`,
`$HOME/.claude/projects`, `$HOME/.gemini/tmp`, and similar source material. A
local `.git` can make unwanted deletions reversible, make repair diffs visible,
prove when signature blobs entered a root, and preserve raw-versus-derived
history. That power is also a retention risk, because content the user expected
to delete can remain reachable in local history.

## Default Mode

- read-only source discovery
- no automatic `.git init`
- no hidden content snapshots
- no remote
- no cloud upload

Use `aicx list` to audit known roots. It reports whether a local git backend is
already present and whether any remotes are configured. It does not modify the
source roots it lists.

## Opt-In Git-Local Backend

Dry run first:

```bash
aicx sources protect --root "$HOME/.codex" --backend git-local
```

Apply explicitly:

```bash
aicx sources protect --root "$HOME/.codex" --backend git-local --apply
```

The `git-local` backend creates a local `.git` only under the requested root. It
adds safe `.gitignore` suggestions unless `--no-gitignore` is passed. It never
creates a remote by default. Peer sync between machines can be useful for a
power user, but that remote must be configured outside AICX as an explicit
operator decision.

To create an initial content snapshot, add `--initial-snapshot`. That commits
the current source-root contents into local history, so use it only when the
retention implications are acceptable.

To remove local source protection, remove the `.git` directory from that source
root after confirming you no longer need its recovery history.

## Future Backend Options

- append-only manifest journal with path, mtime, size, hash, and event metadata
  but no content
- content-addressed snapshots under AICX-owned storage
- SQLite audit log with optional compressed blobs
- encrypted restic/kopia-like snapshot backend

These backends must stay opt-in and must explain what is stored, where it is
stored, and how to remove it.

## Team And Cloud Sharing

Local source journaling is separate from sharing. AICX must distinguish:

- private raw sessions
- cleaned transcripts
- decision, finding, and report artifacts
- context packs
- public or exportable knowledge

Team or cloud sharing needs a separate consent layer, redaction layer, and scope
layer. Raw source roots are private by default and must not be uploaded merely
because local protection is enabled.
