# Workflow Branching

A step declares a condition under which it runs. From that one primitive: either/or
branches, optional steps, guards, switches, and error routing — with merge behaviour
falling out of the input schema that was already there.

## Problem

Three questions about a step are currently squeezed into two mechanisms:

| question | mechanism |
|---|---|
| what order does this run in? | dependency edges |
| is the outside world ready? | gates |
| **should this run at all?** | **nothing** |

The third has no home, so it gets expressed as a gate — and *is CI green yet?* and
*should this branch run?* have the same predicate but **opposite** failure modes.
Waiting forever is correct for the first and a deadlock for the second.

That is not hypothetical. The `deploy-or-rollback` workflow on the dev board has a
`rollback` step gated on `deploy` reporting `red`. Deploy reported `green` and is
`done`, so the gate can never open. `rollback` sits in the backlog permanently,
displayed as though it might still run. Put a merge step below both branches and
the pipeline **deadlocks outright**: every promotion path asks `p.status <> 'done'`
for every parent, and one parent is never going to be done.

This is the same one-label-two-conditions bug that has now caused three separate
incidents in this codebase — the promote/re-block loop, the two meanings of
`backlog`, and the three states a gate can be in.

Separately, and more mundanely: **a template cannot declare a gate at all**.
`task_gates` is keyed by `task_number`, and a template has only step keys. Branching
on the dev board had to be assembled by a script that launched the run, read back the
emitted task numbers, and POSTed gates against them. Branching is therefore a property
of one *run*, not of the template — launch it again and there are no branches.

## The stance

In most workflow engines conditionals must be rich, because the engine is the only
intelligence in the system: the DSL grows expressions, functions, templating, and
eventually a debugger.

Here, **a step can be the condition.** "Does this need legal review?" is a task; a
model answers it and returns `{"needs_legal": true}`. The routing predicate only ever
reads a value something smarter already computed.

So the predicate language stays permanently small — RFC 6901 pointer plus `equals` /
`any_of`, exactly what `task_gates` and `loop_until` already use. **The model reasons;
the graph routes.** Wanting `AND`/`OR`/arithmetic in a predicate is the signal that a
step should be computing it instead.

This is a scope boundary, not a limitation to be lifted later.

## Design

### The primitive

A step declares a **condition**: a predicate plus a **disposition** saying what a false
answer means.

```
wait   — not yet. Poll again. (gate semantics, today's behaviour)
route  — no. This step does not apply; it is settled and will never run.
```

Same table, same evaluator, one new field. The disposition is the entire fix.

### Deriving the disposition

`disposition` is nullable, and null means *derive*:

- source is a **task output** and that task is **terminal** — `done` *or* `skipped` →
  nothing can change the answer → **route**
- source is **http**, or the source task is still able to change → **wait**

This is not a heuristic. It is a fact about whether the input can still change, which
is precisely the thing that distinguishes the two questions.

`skipped` counts as terminal here, and it has to. A condition reading a branch that
was itself skipped would otherwise derive `wait` and hold forever — the same deadlock
this feature exists to remove, one level further down. Terminality is the property
that matters; `done` was an earlier draft of this sentence naming only half of it. It is right nearly always,
which is what makes it a good default.

The override exists for what the derivation cannot see: an `http` gate polling a
decision endpoint that really is final, or a `task_output` condition that should hold
the whole pipeline rather than skip past it. Set at authoring time on the step, because
that is when the author knows.

### `skipped` is a task status

A seventh status, terminal.

Terminal is load-bearing: a task that could un-skip would make "settled" meaningless
and put the sweep straight back into the promote/re-block territory it escaped. There
is deliberately no un-skip operation in v1.

Dependency satisfaction changes from *done* to **settled**:

```sql
-- before
AND p.status <> 'done'
-- after
AND p.status NOT IN ('done', 'skipped')
```

That predicate appears in **9 places** in `src/tasks/store.rs`. Nine hand-copied
literals is how drift happens, so it becomes one shared SQL fragment referenced
everywhere rather than nine edits.

A skipped task records `skip_reason` — its own column, not `block_reason`. Overloading
the block fields would tie skip to the block machinery (recurrence limits, sticky
kinds, unblock) that has nothing to do with it, and that overloading is the exact
pattern this document exists to stop repeating.

### Propagation falls out of the input schema

**A binding whose source task was skipped resolves to *absent*.** The step's own
`input_schema` then decides whether that is legal:

- the input is **required** and its branch skipped → the contract cannot be satisfied →
  **the step skips too**
- the input is **optional** → the step runs without it

So `required` in the JSON Schema **is** the join rule. "Needs both branches" and "needs
at least one" are expressed by writing the schema the author had to write anyway. No
`all`/`any` trigger-rule vocabulary, no new concept, and it reuses validation that is
already implemented and already enforced at claim time.

Absent rather than `null` matters: `null` is a value a model will reason about ("the
review returned null…"), absent means the review never happened.

Propagation is therefore **lazy** — evaluated when a task is considered for promotion,
not as an eager cascade when something is skipped. No separate pass, no ordering bugs,
and a task whose branch skipped is examined exactly once, at the moment the answer
matters.

Mechanically this is a fourth outcome from `resolve_inputs`, alongside `NotRequired`,
`Resolved`, `Unresolved`, and `Pending`:

```
Unreachable { reason }  — a required input's source is settled and produced nothing
```

A step that binds nothing from a skipped parent still runs. It declared a dependency on
that parent's *ordering*, not on its output, and honouring exactly what was declared is
the right behaviour.

### Templates can declare gates

The mechanical half of the original issue, and a prerequisite for all of the above.

`workflow_step_gates` mirrors `workflow_step_bindings` — addressed by `step_key`,
compiled into real `task_gates` rows by `launch()`, with a `task_output` gate's
`source_step_key` translated into that step's compiled task number. The translation
problem and its solution are identical to bindings; this is well-trodden ground.

```sql
CREATE TABLE workflow_step_gates (
    workflow_id       TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    step_key          TEXT NOT NULL,
    gate_key          TEXT NOT NULL,   -- author-named, so an edit is idempotent
    kind              TEXT NOT NULL,   -- http | task_output
    source_step_key   TEXT,            -- task_output: by name, resolved at launch
    config            TEXT NOT NULL,
    label             TEXT,
    poll_interval_secs INTEGER NOT NULL DEFAULT 60,
    disposition       TEXT,            -- NULL = derive; wait | route
    PRIMARY KEY (workflow_id, step_key, gate_key)
);
```

`task_gates` gains the same nullable `disposition`.

### What the poller does

The gate poller already runs each tick. One addition: a gate whose disposition resolves
to `route` and whose predicate is decidedly false sets its task to `skipped` with a
reason naming the pointer and what was found there.

Everything else — backoff, the error limit, the four `GateResult` states — is unchanged.
`erroring` still means *we could not tell*, and must never route a branch; being unable
to reach CI is not the same as CI saying no.

## What you can express

| shape | how |
|---|---|
| either/or | two steps off one parent, mutually exclusive conditions |
| optional step | one conditional step; skip and continue past it |
| guard | "only if approved", "only on the release branch" |
| switch | N steps, N conditions |
| error routing | pairs with the loop `on_exhausted` edge |

## Build order

1. **`skipped` status + settled-instead-of-done.** The risky one. Nine call sites
   collapsed into one shared fragment, `can_transition` updated, `skip_reason` added.
   Everything else is additive on top.
2. **`workflow_step_gates`** + compilation at launch + API. Purely mechanical.
3. **`disposition`**, derived with override, and the poller acting on `route`.
4. **`Unreachable` from `resolve_inputs`** and skip propagation via required inputs.
5. **UI**: skipped rendering, conditions in the step editor, conditional edges drawn
   distinctly on the canvas.

Steps 1–4 are independently testable and 1 is the only one that touches the scheduler's
core predicate.

## Risks

- **A seventh task status.** `@spacedrive/ai` knows five and its `TaskStatusIcon`
  *crashes* on anything else; `TaskList` silently drops unknown ones.
  `interface/src/components/tasks/boardColumns.ts` already contains the pattern for
  handling this, and `designSystemTask.ts` is the adapter that must not pass `skipped`
  through. Every surface rendering a status needs checking before `skipped` can reach
  the UI.
- **The 9 sites.** Missing one produces a task that waits on a skipped parent forever —
  the deadlock this work exists to remove, reintroduced somewhere less obvious. The
  shared fragment is not tidiness; it is the mitigation.
- **`erroring` must not route.** An unreachable endpoint is our problem, not an answer.
  Conflating it with a decided negative would skip branches because DNS failed.

## Out of scope

Expression languages. Computed predicates. Dynamic re-routing beyond skip. Un-skipping.
Any of these arriving would be evidence that a step should have been doing the thinking.
