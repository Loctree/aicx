# Cold start: from zero to full context

An agent that opens a repository cold has to learn four things before its
first move: who it is and where, what the repository looks like, what is
happening right now, and what is red. This page is the daily scenario for
that, with one aicx call carrying everything aicx knows.

## Why this exists

Measured on 2026-10-21: a fresh operator agent needed about 45 tool calls to
orient in this repository. The most important fact of the day came out only
after a human hint: four finished fleet cuts had sat for 30 hours on local
branches the integration branch lacked, and the Founder's assignment was on
the Codescribe bus under another project. Repository-scoped views showed
neither. The scenario below closes those gaps with existing surfaces; there is
no separate `coldstart` command next to `continuity`.

## The four lenses

### Lens 0 — Who am I

```bash
aicx sessions current --json      # this agent's session id, for trailers and handoffs
scutil --get ComputerName          # which machine (macOS; `hostname` elsewhere)
cs-bus --status                    # which Codescribe channel and name you answer to
```

The name you answer to on the bus is the `<name>` Lens 2 needs.

### Lens 1 — Map

```bash
loct context                       # Loctree atlas: hubs, gates, env, risk (vc-init)
```

Structure stays Loctree's job. aicx does not repeat it.

### Lens 2 — Now

One aicx call:

```bash
aicx continuity show -p <org/repo> --as <name> -H 72 --for-inject
```

MCP clients get the same pack from the `aicx_continuity` tool with
`{"project": "<org/repo>", "as": "<name>", "hours": 72, "for_inject": true}`.

The pack renders these sections, in this order:

| Section | Answers | Replaces (2026-10-21) |
|---|---|---|
| HONESTY | what the window, coverage and caps cut; how far to trust the rest | — |
| NOW | open sessions and the human's unresolved requests and decisions | `aicx_search` probes that came back empty |
| PEERS | what each agent session in the window claimed, three claims per session | reading worker reports one by one |
| DECISIONS | decision candidates, human turns first | — |
| TASKS | task candidates, including dispatched checklists | — |
| OPEN WORK | local branches carrying commits HEAD lacks, the worktree holding each, its newest subject, `Authored-By` and `session_id` | `git worktree list` + `git cherry` by hand |
| ADDRESSED TO \<NAME\> | Codescribe bus speech addressed to `<name>`, from any project | `cs-bus --history` per channel |
| SOURCES | the session files behind the retained claims | `aicx sessions list` on a human hint |
| INDEX HEALTH | index freshness, kept apart from continuity coverage | — |

What the sections promise, and what they do not:

- **Dispatch briefs are not human speech.** A worker session that opens with
  the Vibecrafted launcher envelope keeps its checklist as task evidence with
  `attribution=dispatch_brief`; it never ranks as a human decision and never
  counts toward `human=N`.
- **OPEN WORK is local git, read-only.** Checkouts come from the project's
  recorded workdirs, with linked worktrees folded into their principal
  checkout. A row means the commits are missing from HEAD by patch identity:
  a squash merge or a pick re-applied with changed context still lists the
  branch, and `newest_reapplied_in_head=<sha>` names the HEAD commit that
  carries the same subject and `session_id` trailer. That is a hint, not proof
  of integration. Unexamined branches are counted, never read as clean.
- **ADDRESSED TO matches structured recipients and audience only.** A name
  merely mentioned in someone else's message does not match, dictation has no
  addressee and is excluded, and open takes are withheld until sealed.
- **`--for-inject` keeps the pack inside about 6k tokens.** Claim lines are cut
  first, whole; SOURCES and INDEX HEALTH are reserved; the omission marker
  names every shortened section. On a busy machine OPEN WORK and ADDRESSED TO
  can take the whole remaining budget: measured on the live store on
  2026-10-21, the marker named NOW, PEERS, DECISIONS and TASKS and none of
  their lines survived. When the marker names NOW, read the same command
  without `--for-inject` before the first move.
- **72 hours** reaches back over a weekend or a night of fleet work without
  pulling in last month.

When the human's own words matter verbatim, settle them separately:

```bash
aicx utterances -p <org/repo> -H 72
```

It exits 0 only when every session in the window was read whole. Source files
whose mtime is older than the window start minus 24 hours cannot hold an
in-window turn; they are counted as `skipped_before_window` and not read, so
old sessions no longer refuse every recent window.

### Lens 3 — What is red

```bash
gh pr list                                   # open PRs and their checks
gh run list --branch "$(git branch --show-current)" --limit 5   # CI on HEAD
vibecrafted settlements list --bucket n      # fleet runs settled as "needs attention"
```

Network and fleet state stay in this lens (and in `vc-init`). aicx does not
call `gh` or `vibecrafted`; it covers memory and the repository's local truth.

## The card

Read the four lenses into at most ten lines:

1. who and where (agent, session id, machine, channel)
2. whose turn it is (the newest ADDRESSED TO line, the human's open requests in NOW)
3. what is open and whose (OPEN WORK rows with their `session_id`, open sessions in NOW)
4. what is red (PRs, CI, unsettled runs)
5. the first move

## The help lens

`~/.agents/aicx-helps/` holds one file per aicx command and subcommand, in the
same shape as `~/.agents/loct-helps/`: the first line
`########## aicx <cmd> [<sub>] --help ##########`, then the help as the binary
prints it (`_root.txt` holds `aicx --help-full`). Regenerate it after every
install or upgrade so it never describes an older binary:

```bash
tools/dump_helps.sh ~/.agents/aicx-helps            # aicx on PATH
tools/dump_helps.sh ~/.agents/aicx-helps "$AICX_BIN" # a specific binary
```

The script enumerates commands from `--help-full`, hidden power-user surfaces
included. It writes nothing unless every page succeeded, and it removes only
pages it wrote earlier for commands that no longer exist.

## Proof

`tests/coldstart_scenario.rs` rebuilds the 2026-10-21 morning as one fixture:
four unintegrated cut branches with worker `session_id` trailers held by fleet
worktrees, one cut already cherry-picked, worker sessions opened by a dispatch
brief, a Founder take on the bus addressed to the operator agent under another
project, and junie sessions from April to September that cannot be read
whole. It runs the Lens 2 command above through the real binary (with a frozen
`--until` and `--no-refresh`), checks `aicx utterances` on the same fixture,
regenerates the help lens with `tools/dump_helps.sh`, and pins this page to
the exact command it runs.

```bash
cargo test --locked -p aicx --test coldstart_scenario
```

_𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI_
