# Command Steps

A workflow step that runs a command instead of an agent. `bun run lint` is not a
question anybody needs a model to answer.

## Problem

Every step in a workflow compiles to a task, and every task is claimed by a worker —
an LLM agent with a full tool loop. For "summarise these findings" that is exactly
right. For "does this lint" it is wrong three ways:

- **Cost.** A model turn to run a command and report what it printed.
- **Latency.** Roughly a minute against roughly a second.
- **Truth.** This is the one that matters. Asked whether the code lints, a model
  reports *its account* of the exit code. A step whose whole purpose is to be an
  objective check should not route its answer through something that can be
  mistaken about it.

That third point is what makes this more than an optimisation. The loop and branch
predicates read step outputs and decide what runs next. A predicate is only as
trustworthy as the value it reads, so a **deterministic check makes every downstream
decision trustworthy**. `{"exit_code": 0}` is ground truth; "I ran the linter and it
looked clean" is testimony.

## Design

### A step kind

`workflow_steps` gains `kind`: `agent` (default, today's behaviour) or `command`.

A command step carries a command line, and produces:

```json
{"exit_code": 1, "stdout": "…", "stderr": "…", "duration_ms": 840}
```

That is its `output_schema`, implicitly — bindings, gates, `loop_until` and conditions
all read it with the pointers they already use. No new plumbing anywhere downstream,
because a command step is just a task that produces outputs like any other.

### Where it runs

Nowhere new. A task already binds to a project / repo / worktree, and
`resolve_worker_working_dir` already resolves that binding to a directory and refuses
anything the sandbox allowlist does not cover. A command step runs in exactly the
directory its binding names, under exactly the same rule, and a step with no binding
has no directory and is refused rather than silently defaulting to the workspace.

### Exit code is data, not failure

**The load-bearing decision.** These are two different events:

| | |
|---|---|
| the command **ran and reported a problem** | `exit 1` from a linter — the *task* succeeded |
| the command **could not run** | binary missing, timeout, killed — the *task* failed |

Conflating them breaks the entire feature. A lint step that treats `exit 1` as a task
failure burns two attempts of the failure budget and parks itself before the fix loop
has run twice — the loop would die of the very condition it exists to fix.

The distinction is **derivable, not configured**, which is the same shape as the gate
disposition in `workflow-branching.md`. At the process level:

- `spawn()` errored, the timeout fired, or the process was signalled → **task failure**,
  charge the budget
- the process ran to completion and exited → **task success**, whatever the code, and
  the code is data

An optional `expect_exit_code` exists for steps where non-zero really is a failure —
`git push` should not quietly "succeed" with exit 1. Absent by default, because the
common case is a check whose answer is the point.

### The loop this exists for

```
  lint ──▶ fix ──▶ lint'
   │                │
   │        loop_until /exit_code == 0
   │        max_iterations 3
   ▼
 (clean)
```

Concretely: a `command` step running the linter, an `agent` step bound to the previous
iteration's `stdout` with instructions to fix what it reports, and a `loop_group` over
the two with the check as the body's exit step.

Two things fall out that were designed before this was on the table:

- **`loop_until` reads the check, not the fixer.** The body's exit step is the
  deterministic one, so the loop terminates on ground truth rather than on the fixer's
  opinion of its own work. That is the whole reason the exit predicate belongs on the
  body's terminal step.
- **`PreviousIteration` falling back to the entry binding on iteration 1** means the
  fixer reads the pre-loop lint on its first pass and the previous iteration's lint
  after that, with no special first-pass wiring. That fallback was specified for loops
  in the abstract; this is the case that justifies it.

### Output is capped, and says so

`stdout` feeds the next step's prompt, so it is both the point and the risk. Capped at
a fixed size with an explicit marker when truncated — silently handing a model half a
log and no indication is how a fix loop ends up confidently fixing the wrong thing.
Head and tail are kept in preference to the head alone; the useful part of a failing
build log is usually at the end.

### Security

A command step is arbitrary code execution, stored in a template, run repeatedly and
unattended. That is a meaningfully different exposure from a worker choosing to run a
command in the moment, even though the capability is the same.

Nothing new is invented for it. It reuses `Sandbox::wrap`, the read/write allowlists,
the cwd rule above, `kill_on_drop` so a timeout cannot orphan a process tree, and a
hard timeout that is a required field rather than an inherited default. Tool secrets
are *not* injected into the environment: a worker gets them because a worker is trusted
to use them, and a template command is authored once and run forever.

**But `Sandbox::wrap` contains less than its name suggests, and this matters here.**
`containment_active()` is `mode_enabled() && backend != None`, where the backend is
bubblewrap on Linux or `sandbox-exec` on macOS. With no backend installed — which is
the case on the current preview host — `mode = "enabled"` in config yields *no OS-level
containment at all*: the read/write prompt allowlists come back empty and `wrap` builds
an ordinary `Command` with a `PATH` adjustment. What remains is `is_path_allowed`,
which the *file tools* consult voluntarily and which a shell command does not go
through.

For a worker that is a considered risk: an agent runs a command in the moment, watched,
as part of work someone asked for. A command step is different in kind — stored,
repeated, and unattended — so it should not inherit that posture silently. A command
step must **refuse to run when containment is inert**, unless the instance explicitly
opts out. Failing closed is the only honest default for stored code execution, and it
also turns an invisible property into a visible one.

That the two conditions read alike from the config surface — `mode_enabled()` says yes,
`containment_active()` says no — is the same one-label-two-conditions shape this
codebase keeps paying for, this time in the security layer.

## What it does not become

Not a general job runner. No retries of its own (the failure budget already exists),
no cron (that already exists), no long-running services. A command step runs, exits,
and produces outputs — anything that wants to be a daemon is not a step in a pipeline.

Nor is it a way to avoid agents. The interesting pipelines are mixed: a deterministic
check, an agent that reasons about the output, a deterministic verification that it
worked. **The value is in the alternation** — each doing the thing the other cannot.

## Build order

1. `kind` on `workflow_steps`, command line and timeout, launch validation (a command
   step must have a binding; an agent step must not carry a command line).
2. The pickup branch in `cortex.rs`: a command task executes rather than spawning a
   worker. This is the only scheduler change.
3. Outputs, the ran-vs-failed distinction, the output cap.
4. UI: a visibly different node on the canvas, because a graph that draws a shell
   command identically to an agent step is lying about what it does.

## Risks

- **The ran-vs-failed distinction is the whole feature.** Getting it backwards makes
  every check step burn its failure budget on a working check.
- **A command step is not a worker**, so everything the worker path does incidentally —
  attempt records, event emission, the reaper — has to be done deliberately or a
  command task becomes invisible to the machinery that recovers dead work.
- **Prompt injection through stdout.** A fix step reads a build log written by
  whatever the build touched. It arrives as task input, and task input is already
  treated as data rather than instructions, but this is the first path where the
  content is attacker-influenceable at scale.
