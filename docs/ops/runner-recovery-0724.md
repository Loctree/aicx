# Self-hosted Runner Recovery — 2026-07-24

Incident runbook for the `ops-linux` + `dragon-macos` self-hosted runner
outage that let PRs merge over a non-gating merge queue.

## What the API proves (hard facts, not guesses)

Merge commit `ac82421` (PR #42), push-CI run `30083809370`:

| job | runner_id | runner_name | steps run | conclusion |
|---|---|---|---|---|
| linux-self-hosted | **0** | **""** | 0 | cancelled |
| macos-self-hosted | **0** | **""** | 0 | cancelled |
| windows-latest (hosted) | 1000005279 | GitHub Actions … | 7 | cancelled mid-`cargo check` |

`runner_id: 0` + empty `runner_name` = **no self-hosted runner ever accepted
the job**. Not "picked up and stalled" — never assigned. The runners were
**offline / deregistered** at 09:46Z. The job sat unassigned and was
cancelled ~5 min later when the next event superseded it.

- Last fully-green self-hosted CI on `main`: `3eda941` (2026-07-15).
- Runners are **org-level** (`Loctree`): repo shows `total_count: 0`; org
  runner API needs `admin:org`. Check status in
  **Org → Settings → Actions → Runners**, or on the hosts.

## Two failure modes — different signatures

| Hypothesis | API signature it produces | Matches 0724? |
|---|---|---|
| **SSD full** → runner service crashes/stops | runner offline → `runner_id: 0`, no pickup | ✅ yes — this is what we see |
| **Target cleaner ate cargo binaries / toolchain PATH drift** | runner **online**, job starts, **fails in `cargo check`** | ❌ no — build never started |

The 0724 outage is **runner-offline**, consistent with disk-full crashing the
service. The cleaner/PATH problem is a **real second bug** with a *different*
signature (red build under a live runner) — fix it too, or it bites the
moment the runner comes back.

---

## Step 0 — confirm the symptom

Org → Settings → Actions → Runners. Are `ops-linux` and `dragon-macos`
**Offline**? If yes → this runbook. If **Idle/Active** but jobs still fail →
skip to Mode B.

## Mode A — SSD capacity (primary suspect)

Run on **each** host (dragon-macos, ops-linux):

```bash
df -h .                                   # runner volume free space
du -sh ~/actions-runner/_work/* 2>/dev/null | sort -h | tail       # _work bloat
du -sh ~/.cargo ~/.rustup ~/Library/Caches 2>/dev/null | sort -h   # toolchain/cache
# macOS: APFS container pressure
diskutil apfs list | grep -A2 Capacity 2>/dev/null || true
```

Remediation when full:
- Purge stale `target/` in `_work` build dirs — **but** exclude the toolchain
  and runner internals (see Mode B — the cleaner is the likely culprit).
- Clear old artifacts / caches, not `~/.cargo/bin` or `~/.rustup`.
- After freeing space → restart the runner service (Step R).

## Mode B — target cleaner + toolchain PATH

Find the cleaner ("roast target cleaner" that was destroying cargo binaries):

```bash
# cron + service-managed timers
crontab -l 2>/dev/null; sudo crontab -l 2>/dev/null
launchctl list 2>/dev/null | grep -iE 'clean|sweep|cargo|target'          # macOS
ls -la ~/Library/LaunchAgents /Library/LaunchDaemons 2>/dev/null | grep -iE 'clean|sweep|cargo'
systemctl list-timers 2>/dev/null | grep -iE 'clean|sweep|cargo'          # linux
```

Verify it is **not** deleting the toolchain. It must scope to `target/`
dirs only (e.g. older-than-N-days) and **exclude**: `~/.cargo/bin`,
`~/.rustup`, `~/actions-runner` internals.

Toolchain PATH **in the runner service env** (NOT your login shell — dragon
uses its own toolchain and the service captured PATH at install time):

```bash
cat ~/actions-runner/.path 2>/dev/null     # PATH the service actually uses
cat ~/actions-runner/.env  2>/dev/null
# does cargo resolve in that PATH?
env -i PATH="$(cat ~/actions-runner/.path 2>/dev/null)" bash -lc 'which cargo; rustup show' 2>&1
```

If `cargo`/`rustc` don't resolve in the service PATH → re-point `.path` (or the
launchd plist / `runsvc.sh`) at the toolchain dir dragon actually uses, then
restart the service.

## Step R — restart + verify green

```bash
# from ~/actions-runner
./svc.sh status          # macOS/linux
./svc.sh stop && ./svc.sh start
# linux systemd alt: sudo systemctl restart 'actions.runner.*'
```

Confirm **Idle** in Org → Runners. Then force one real CI run and confirm
`runner_name` is populated and jobs go green:

```bash
gh workflow run ci.yml --ref main            # or push a trivial commit
gh run watch $(gh run list --workflow ci.yml --branch main --limit 1 --json databaseId --jq '.[0].databaseId')
```

---

## Hardening — so this can't silently repeat

**1. Make the merge queue actually gate.** Ruleset `main protection`
(id `16720745`) has a `merge_queue` rule but **no `required_status_checks`** —
that is why a red gate still merged. Land this ONLY after runners are green
(else it bricks every merge). Operator button:

```bash
gh api repos/Loctree/aicx/rulesets/16720745 --jq '.rules' > /tmp/rules.json
jq '. + [{
  "type":"required_status_checks",
  "parameters":{
    "strict_required_status_checks_policy":true,
    "do_not_enforce_on_create":false,
    "required_status_checks":[
      {"context":"cargo check (workspace)"},
      {"context":"Bundle + dry-run publish (darwin-arm64)"},
      {"context":"Bundle + dry-run publish (linux-x64-gnu)"},
      {"context":"Bundle + dry-run publish (win32-x64-gnu)"}
    ]
  }
}]' /tmp/rules.json > /tmp/rules-new.json
gh api --method PATCH repos/Loctree/aicx/rulesets/16720745 \
  -f name='main protection' --input <(jq '{rules: .}' /tmp/rules-new.json)
```

(Contexts are the `merge-queue-gate.yml` job names — those run on
`merge_group`, which is what the queue waits on. `ci.yml` runs on
push/pull_request, not merge_group, so its contexts do not gate the queue.)

**2. Runner disk + health alert.** A heartbeat / low-disk alarm on both hosts
so an offline runner surfaces in minutes, not 9 days.

**3. Scope the cleaner.** Exclude `~/.cargo/bin`, `~/.rustup`, `~/actions-runner`
from whatever sweeps `target/`.
