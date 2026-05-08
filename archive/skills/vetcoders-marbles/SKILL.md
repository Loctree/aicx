---
name: vetcoders-marbles
version: 1.0.0
description: >
  Iterative convergence skill — the noise scheduler for code.
  Runs adaptive denoising loops until the product surface converges from
  chaos to completeness: P0=0, P1=0, P2=0 — the circle is full.
  Each loop reduces entropy: implement → followup → measure → repeat.
  Based on the insight that agent-generated code follows diffusion dynamics:
  start from noise, iteratively denoise, converge to signal.
  Trigger phrases: "marbles", "loop until done", "fill the gaps", "kulki",
  "iteruj aż będzie gotowe", "convergence loop", "denoise", "dyfuzja",
  "noise schedule", "adaptive loops", "keep going until clean",
  "wypełnij okrąg", "entropy reduction", "denoising sprint".
---

# vetcoders-marbles — Iterative Convergence Through Diffusion

> Code is noise until proven signal.
> Each loop removes entropy. Stop when the circle is full.

## The Metaphor (and the Math)

Imagine a circle of radius 10. You throw balls of diameter 1.
First throw: ~60% coverage. Random placement, big gaps.
Second pass: target the gaps. ~80%.
Third: ~90%. Fourth: ~95%.

This is not failure. This is **convergence**.

AI agents generate code stochastically — next-token prediction
produces an approximation, not a proof. Each generation introduces
signal AND noise. The only way to separate them: iterate, measure, reduce.

This is isomorphic to diffusion models:

```
Image diffusion:  noise → denoise × N → image
Code diffusion:   chaos → reduce_entropy × N → product

Step 0:  Pure noise (no context, no structure)
Step 1:  Init — gross shapes emerge (memory + eyes)
Step 2:  Implement — detail generation (new noise enters!)
Step 3:  Followup — denoising (measure residual entropy)
Step N:  Converged — DoU score below threshold = DoD
```

## When To Use

- After first implementation pass leaves known gaps
- When followup reveals P1/P2 findings that need iterative fixing
- When the team says "keep going until it's clean"
- When Plague Score > 20 after first hydration
- Anytime the answer to "is it done?" is "almost"
- When you need adaptive iteration count (not fixed 2 loops)

## The Noise Schedule

### No Fixed Loop Count

There is no fixed number of iterations. The loop runs until the circle
is full: P0=0, P1=0, P2=0. A trivial task may converge in 1 loop.
A massive architectural change may take 12. The schedule is driven by
measurement, not prediction.

```
The loop count is determined by residual entropy, not upfront estimation.
After each loop, measure what remains. If P0+P1+P2 > 0, loop again.

Factors that affect how many loops a task needs:
- LOC changed in first pass
- Number of files touched
- Number of P0+P1+P2 from first followup
- Blast radius from loctree impact()
```

### Step Size (Learning Rate Analogy)

Early loops: large steps (fix P0 blockers, structural issues).
Late loops: small steps (P2 polish, edge cases, naming).

```
Early loops:  Fix P0 and P1 — big structural corrections
Middle loops: Fix remaining P1 and high-impact P2
Late loops:   Polish P2 — edge cases, error handling, naming, docs
```

Do NOT spend early loops on P2 polish.
Do NOT spend late loops on structural changes (that's a new diffusion run).

## Convergence Protocol

### Each Loop Iteration

```
┌─────────────────────────────────────────────────────────┐
│  LOOP N                                                  │
│                                                          │
│  1. MEASURE residual entropy                             │
│     └─ Read previous loop's findings                     │
│     └─ Run quality gates                                 │
│     └─ Count: P0 remaining, P1 remaining, P2 remaining  │
│                                                          │
│  2. TARGET the gaps (not random — guided by findings)    │
│     └─ Write focused fix plan for TOP findings only      │
│     └─ Max 3-5 items per loop (don't boil the ocean)     │
│                                                          │
│  3. IMPLEMENT fixes                                      │
│     └─ vetcoders-implement (safe) or spawn (power)       │
│     └─ Each fix is a marble thrown at a known gap         │
│                                                          │
│  4. DENOISE (followup on this loop's changes)            │
│     └─ Run gates on changed files                        │
│     └─ Verify fixes didn't introduce new noise           │
│     └─ Update findings list                              │
│                                                          │
│  5. SCORE                                                │
│     └─ Calculate convergence metrics                     │
│     └─ Decide: continue or converged?                    │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### Convergence Metrics

After each loop, calculate:

```markdown
## Loop N Convergence Report

Entropy remaining:
- P0 count: X (must be 0 to converge)
- P1 count: X (must be 0 to converge)
- P2 count: X (must be 0 to converge — full circle fill)

Quality gates:
- Build: pass/fail
- Lint: pass/fail
- Tests: X/Y passing
- Security: pass/fail

Coverage delta:
- New code covered by tests: X%
- Files touched this loop: N
- Net LOC delta: +X / -Y

Convergence score: X/100
- 0-30: heavy noise, continue with large steps
- 30-60: converging, continue with medium steps
- 60-85: nearly converged, small polish steps
- 85-99: close — keep going, resolve remaining P2
- 100: converged — circle is full, stop iterating
```

### Stopping Criteria

**STOP iterating when ANY of these are true:**

1. **P0 = 0 AND P1 = 0 AND P2 = 0** — the circle is full
2. **Convergence score = 100** — all findings resolved
3. **Two consecutive loops with zero delta** — plateaued (reassess remaining items)
4. **User says stop** — always respected

**DO NOT STOP when:**
- P0 > 0 or P1 > 0 (unless user explicitly accepts risk)
- P2 > 0 (the circle is not full — keep iterating)
- Quality gates failing
- Last loop introduced more findings than it fixed (diverging!)

### Divergence Detection

If loop N has MORE findings than loop N-1:

```
WARNING: DIVERGENCE DETECTED

Loop N-1: 3 P1, 5 P2
Loop N:   4 P1, 7 P2  ← entropy increased!

This means fixes are introducing new noise faster than removing old noise.
Possible causes:
1. Scope too broad — narrow the fix scope
2. Wrong abstraction — step back and re-examine
3. Living tree conflict — other changes interfered
4. Agent hallucination — verify the "fix" actually fixed anything

Action: STOP. Re-run vetcoders-workflow (Examine phase) on affected area.
Do not continue blind iteration on a diverging trajectory.
```

## Implementation Pattern

### Using vetcoders-implement (native, safe)

```
For each loop:
  1. Read previous findings
  2. Select top 3-5 actionable items
  3. Launch parallel Task agents:

     Task("Fix: <finding-1>", prompt=<focused fix plan>)
     Task("Fix: <finding-2>", prompt=<focused fix plan>)
     Task("Verify: run gates", prompt="cd $ROOT && <gate commands>")

  4. Collect results
  5. Write loop report to .ai-agents/pipeline/<slug>/reports/loop_N.md
  6. Calculate convergence score
  7. If not converged → loop N+1
```

### Using vetcoders-spawn (Terminal, power)

```
For each loop:
  1. Write loop plan to .ai-agents/pipeline/<slug>/plans/loop_N_fixes.md
  2. Spawn agent with plan
  3. Read report from .ai-agents/pipeline/<slug>/reports/loop_N.md
  4. Run gates locally
  5. Calculate convergence score
  6. If not converged → loop N+1
```

## Output Format

### Per-Loop Report

```markdown
# Marbles Loop N: <slug>
Date: <YYYY-MM-DD>
Duration: <time>

## Entropy Before
- P0: X | P1: X | P2: X
- Convergence score: X/100

## Marbles Thrown (fixes applied)
1. [P1] <finding> → <fix applied> → <result>
2. [P2] <finding> → <fix applied> → <result>
3. [P1] <finding> → <fix applied> → <result>

## Entropy After
- P0: X | P1: X | P2: X
- Convergence score: X/100

## Gate Results
- Build: pass/fail
- Lint: pass/fail
- Tests: X/Y
- Security: pass/fail

## Delta
- Findings fixed: N
- Findings introduced: N
- Net entropy change: -N (negative = good)

## Decision
- [ ] Continue → Loop N+1 (reason: <what remains>)
- [ ] Converged → proceed to DoU
- [ ] Diverging → stop and re-examine
```

### Final Convergence Report

```markdown
# Marbles Convergence: <slug>
Date: <YYYY-MM-DD>
Total loops: N
Total duration: <time>

## Trajectory
| Loop | P0 | P1 | P2 | Score | Delta |
|------|----|----|----|----|-------|
| 1    | 2  | 5  | 8  | 15 | —     |
| 2    | 0  | 3  | 6  | 40 | +25   |
| 3    | 0  | 1  | 4  | 65 | +25   |
| 4    | 0  | 0  | 2  | 85 | +20   |
| 5    | 0  | 0  | 0  | 100| +15   |

## Convergence Curve
Score: 15 → 40 → 65 → 85 → 100
       ████████████████████████████████████████████████

## Final State
- All P0: resolved
- All P1: resolved
- All P2: resolved
- Quality gates: all passing
- Circle: full

## Verdict
DoU → DoD transition: COMPLETE
Plague Score before: XX → after: XX
Ready for: Phase 3 (dou → hydrate)
```

## The DoU → DoD Transition

This is the moment the circle is full:

```
DoU (Definition of Undone) = measuring remaining noise
DoD (Definition of Done)   = circle is full, no noise remains

The transition happens when:
- P0 = 0, P1 = 0, P2 = 0
- Convergence score = 100
- Quality gates pass
- Stranger test passes (someone unfamiliar can use it)

At this point, DoU transforms into DoD:
  "What remains incomplete?" → "Nothing."
  ~~DoU~~ → **DoD**
```

## Integration with VetCoders Pipeline

```
Phase 1 — Build:     init → workflow → followup
                                         ↓
Phase 2 — Converge:                  marbles ↻ (loop until P0=P1=P2=0)
                                         ↓
Phase 3 — Ship:                      dou → hydrate
```

Marbles is the gate between building and shipping.
It does not loop back to workflow. It loops itself.
implement/spawn are internal execution tools used by workflow and marbles.

## Anti-Patterns

- Fixed loop count ("always run 4 loops") — defeats adaptive scheduling
- Looping without measuring (no convergence score = blind iteration)
- Fixing P2 before all P0 are resolved (wrong step size)
- Continuing past convergence (overfit — introduces new noise)
- Looping without writing reports (no convergence trajectory = no learning)
- Ignoring divergence detection (if entropy increases, STOP)
- Single marble per loop (too slow — throw 3-5 per loop)
- Entire codebase per loop (too broad — scope to affected area)

## The Diffusion Insight

Why this works:

1. **Next-token prediction is stochastic** — agents will always produce noise
2. **Noise is not failure** — it's a natural property of the generation process
3. **Denoising is the skill** — measuring and removing noise iteratively
4. **Convergence is achievable** — each loop provably reduces entropy
5. **The schedule matters** — too few loops = noisy output, too many = wasted time
6. **Divergence is real** — detect it early, don't iterate blindly

The marbles fill the circle. Not "enough." All the way.
P0=0. P1=0. P2=0. Quality gates pass. The circle is full.

That's DoD. Not "good enough." Done.

---

*"Code is noise until proven signal.*
*Each loop removes entropy.*
*Stop when the circle is full."*

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
