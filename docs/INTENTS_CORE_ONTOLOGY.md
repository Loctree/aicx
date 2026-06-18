# AICX Intents Core Ontology

This document is the target semantic contract for AICX intent extraction. It is
not a record of current implementation behavior. For current runtime behavior,
see `docs/INTENTS_CLASSIFICATION_RULES.md`.

The goal is to make AICX distinguish core conversation facts before any
user/project calibration layer is considered.

## Scope

This contract covers the core ontology:

- `intent`
- `task`
- `decision`
- `outcome`
- adjacent non-bucket roles that must not be confused with those buckets

User dictionaries, operator idioms, and project-specific vocabulary are parked
for a later calibration layer. They must not rescue a weak ontology.

## Principle

AICX must classify by semantic role, not by emotional force.

Strong language such as `musimy`, `trzeba`, `ma byc`, or `nie moze byc` can
raise importance, but it must not by itself turn a request into a decision or a
work item into an outcome.

## Core Buckets

### `intent`

An `intent` is a desired direction, need, problem to solve, proposed change, or
question/motivation that explains what should become true.

It answers:

- What does the operator/user want?
- What direction are we exploring?
- What problem or gap has been named?
- Why might work be needed?

Typical signals:

- "we need to..."
- "musimy..."
- "trzeba..."
- "I want..."
- "I would like..."
- "proponuje..."
- "czy da sie..."
- "why / because / root cause" when used to motivate work

Boundary:

- An intent may imply work, but it is not automatically a task.
- An intent may contain forceful language, but it is not automatically a
  decision.
- A question can be an intent when it points at a needed investigation.

Example:

```text
musimy dodac parser dla obcych md
```

Expected bucket: `intent`.

Reason: it names a needed direction. It does not yet bind an implementation
choice or report completed evidence.

### `task`

A `task` is a concrete work item that can be put on an execution queue without
inventing the missing action.

It answers:

- What should someone do?
- Is there a concrete deliverable or action?
- Can this be tracked as open / done / blocked?

Typical signals:

- imperative requests: "stworz plik", "dodaj test", "napraw parser"
- explicit assignment: "zrob X", "please implement Y"
- checklist syntax: `[ ]`, `[x]`
- direct command/request that has an actionable object

Boundary:

- `task` is not limited to checklist syntax.
- `task:` or `zadanie:` should be task when the content is actionable.
- A vague need remains an intent until it becomes executable.
- A promise is not a task unless it also creates a trackable assigned work item.

Example:

```text
stworz prosze plik, w ktorym wypiszesz zasady klasyfikacji
```

Expected bucket: `task`.

Reason: it asks for a concrete deliverable.

### `decision`

A `decision` is a durable choice, constraint, default, rejection, or accepted
tradeoff that should govern later work.

It answers:

- What has been chosen?
- What is now forbidden or required?
- What default/constraint should future agents respect?
- What tradeoff was accepted?

Typical signals:

- explicit tags: `decision:`, `[decision]`
- "we choose X"
- "from now on X"
- "X is canonical"
- "default is X"
- "do not do Y"
- "Y is out of scope"
- "we accept tradeoff A over B"
- declarative constraints about process or architecture

Boundary:

- A strong request is not a decision.
- A complaint is not a decision unless it declares a future constraint.
- A decision does not need to be phrased with `must`; it can be plain declarative
  policy.

Example:

```text
obce md importujemy tylko przez operator-md, bez zgadywania cwd
```

Expected bucket: `decision`.

Reason: it states a durable import policy and forbids a behavior.

### `outcome`

An `outcome` is observed evidence, result, status, or completed-state report.

It answers:

- What happened?
- What did runtime/tests/a batch/a workflow show?
- What is now known from evidence?
- What changed state?

Typical signals:

- tests passed/failed
- counts, scores, latencies, warnings/errors
- "batch produced N records"
- "smoke green"
- "cargo test passed"
- "implementation landed"
- "file was created"
- result/status reports with concrete evidence

Boundary:

- A promise to do work is not an outcome.
- "done" without detail is often just acknowledgement, not useful evidence.
- Natural-language outcome should not require an explicit `result:` tag when it
  carries concrete evidence.

Example:

```text
batch 10 plikow dal 66 records: 48 intent, 14 outcome, 4 decision
```

Expected bucket: `outcome`.

Reason: it reports observed extraction results.

## Adjacent Roles

### Promise / Commitment

A promise or commitment says that an actor intends to do something later.

Examples:

```text
zrobie to zaraz
I'll implement this next
```

This is not an outcome, because it has not happened. It is not automatically a
task either, because it records commitment language rather than the operator's
work item. Depending on speaker and context, it may become:

- a `task` if it creates a trackable assigned work item
- a separate internal `commitment`/`claim` lane
- ignored if it is conversational filler

AICX must not treat promises as proof.

### Claim

A claim is an assertion about reality that may need verification.

Examples:

```text
the parser already supports operator-md
the task lane only reads checklists
```

Claims are adjacent to intents but not the same thing. A false claim can create
an intent or task, but the claim itself should stay auditable as a claim.

### Assumption / Hypothesis

An assumption is a provisional belief used to move work forward. It is not a
decision unless explicitly accepted as a governing constraint.

### Insight

An insight names understanding. It is not automatically a decision or outcome,
though it may explain either.

### Argument / Tradeoff Discussion

Argumentation compares paths. It becomes a decision only when one path is chosen
or rejected.

## Classification Precedence

Target precedence:

1. Explicit structured tags and syntax.
2. Concrete runtime/evidence signals for `outcome`.
3. Durable policy/choice/constraint signals for `decision`.
4. Concrete actionable deliverable signals for `task`.
5. Need/direction/question/motivation signals for `intent`.
6. Adjacent roles: promise, claim, assumption, insight, argument.
7. Low-confidence fallback heuristics.

Precedence must remain explainable. If a rule changes the bucket, AICX should be
able to report which rule did it.

## Golden Examples

These examples are the calibration seed for future classifier tests.
The same seed is stored as JSON in
`tests/fixtures/intents_core_ontology_goldens.json`.
Run the current-vs-target audit with
`cargo test core_ontology_goldens_match_target_semantics -- --ignored --nocapture`.

| ID | Speaker | Text | Expected | Not | Reason |
|---|---|---|---|---|---|
| G01 | user | `musimy dodac parser dla obcych md` | `intent` | `decision` | strong need, no durable choice |
| G02 | user | `trzeba sprawdzic outcomes i claims` | `intent` | `decision` | investigation need |
| G03 | user | `stworz prosze plik z zasadami klasyfikacji` | `task` | `intent` | concrete deliverable |
| G04 | user | `[ ] stworz prosze plik z zasadami klasyfikacji` | `task` | `intent` | explicit open work item |
| G05 | user | `task: stworz plik z zasadami klasyfikacji` | `task` | `intent` | typed task directive with deliverable |
| G06 | user | `obce md importujemy tylko przez operator-md, bez zgadywania cwd` | `decision` | `intent` | durable constraint |
| G07 | user | `slownik ma byc dodatkiem, nie kolem ratunkowym dla ontologii` | `decision` | `intent` | product principle / constraint |
| G08 | assistant | `zrobie to zaraz` | `commitment` | `outcome` | promise, no evidence yet |
| G09 | assistant | `plik docs/INTENTS_CLASSIFICATION_RULES.md zostal dodany` | `outcome` | `promise` | completed-state report |
| G10 | user | `batch 10 plikow dal 66 records: 48 intent, 14 outcome, 4 decision` | `outcome` | `intent` | observed extraction result |
| G11 | user | `czy to jest mozliwe?` | `intent` | `task` | question/investigation need |
| G12 | assistant | `zakladam, ze cwd mozna wywnioskowac z tresci` | `assumption` | `decision` | provisional belief |
| G13 | assistant | `wniosek: frame_kind domyslnie filtruje user_msg` | `insight` | `decision` | understanding, not policy |
| G14 | user | `nie fixujemy tego teraz` | `decision` | `task` | explicit scope rejection |
| G15 | user | `nie moze byc tak, ze task oznacza tylko checklisty` | `intent` | `decision` | complaint/requirement pressure unless paired with policy |

## Parked: User Calibration

User/project vocabulary is real and should be supported later, but only as an
auditable calibration layer.

Parked idea:

- examples and phrase hints can adjust confidence
- calibration can demote/promote only with visible provenance
- every calibration effect should expose `rule_id` / `source=user_calibration`
- calibration must not replace the core ontology

Non-goal for now:

- no freeform keyword dictionary as a first fix
- no magic `word -> bucket` override that hides bad base semantics

The core classifier must first know the difference between a task, a decision,
an outcome, an intent, a promise, and a claim.
