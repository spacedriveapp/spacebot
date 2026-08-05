# Unattended Operation

What a pipeline engine needs before it can be left alone: a run that knows how it is
going, a ceiling on what it can spend, and something other than a person to start it.

Covers tasks #25 (spend), #26 (run state), #27 (triggers). One document because they
are one problem — a spend ceiling has to park a run, a trigger has to report one, and
neither is possible while a run has no state.

## Problem

The graph engine is complete: sequential, parallel, fan-in, dynamic fan-out, bounded
loops with separate success and give-up paths, external gating. All of it verified
against a live model. And none of it can be left running.

Three things are missing, and each was survivable until the other two arrived.

### A run has no state

`workflow_runs` is `(id, workflow_id, inputs, launched_by, created_at)`. There is no
status and no `finished_at`. Every caller that wants to know how a run is going loads
all its tasks and reduces them, differently.

The question that actually matters is not "did it succeed" but **"is it stuck"**, and
nothing can answer it. A loop whose body task is permanently blocked never reaches
"all done", so no boundary fires and nothing announces the loop is wedged — reported
by the loops implementation as its own known gap. A task parked behind a gate that
will never open looks exactly like one waiting normally. The run simply stops making
progress, silently, and stays that way until somebody looks.

There is also no way to cancel a run, and no way to delete one — two separate agents
left empty run rows behind because cleanup had no endpoint to call.

### Nothing bounds what a run costs

Fan-out branch count is **uncapped**. Loops cap at 25 iterations. They multiply.

This was fine while a template compiled to a fixed number of tasks at launch. Both
fan-out and loops grow the graph *after* launch, so the size of a run is now decided at
run time — by model output. A scan step that hallucinates a 900-element array is a
900-task fan-out, each one a model call. Inside a loop, again per pass.

The per-task failure budget bounds *failures per task*. Nothing bounds the number of
tasks a run creates or what the run costs in total. The instance already runs
unattended on a timer, so this is live, not theoretical.

### Nothing can start a workflow but a person

The only non-test caller of `WorkflowStore::launch` is the HTTP handler. So:

- **No schedule.** Cron exists for agents and cannot launch a workflow.
- **No external trigger.** Gates can *wait* on CI; CI cannot *start* anything. The
  gitops loop this was built for does not close.
- **No agent can launch one.** There is a `task_create` tool and no `launch_workflow`
  tool. The cortex can file a card and cannot run a pipeline.

That third is the sharpest: workflows are reusable procedures that the part of the
system meant to be autonomous cannot reuse. An agent deciding "this needs the full
release process" must re-derive the steps by hand every time.

## Design

### Run state

```
running    tasks outstanding, progress recent
succeeded  every task settled, no failure path taken
failed     a task exhausted its budget, or a loop routed to on_exhausted
stuck      nothing running, nothing runnable, not finished
cancelled  a person stopped it
```

`stuck` is the one worth building the rest around. It is not derivable from any single
task — every task can look individually reasonable while the run as a whole cannot
advance. It is a property of the run, which is exactly why the run needs state of its
own rather than a reduction over tasks.

Detection is the same shape as the existing reaper: a periodic pass asking whether a
run has any task in flight, any task promotable, and any unsettled gate that could
still open. None of the three, and not finished, means stuck — with the reason
attached, because "stuck" alone sends someone reading rows.

`finished_at` and a terminal status close a run for good. A cancelled run marks its
unstarted tasks `cancelled` and leaves running ones to finish or be reaped, because
killing work mid-flight loses whatever it had done.

**Notify on transition, not on state.** A run that goes to `stuck` or `failed` should
say so once. Polling a status nobody watches is the same as having no status.

### Spend ceilings

Two limits, both refusing rather than truncating:

- **Fan-out width.** A cap on branch count, refused at expansion with the pointer and
  the count found. A silently truncated fan-out is worse than a refusal, because the
  downstream fan-in aggregates a subset and reports it as the whole.
- **Run task ceiling.** Total tasks a run may create. Reaching it parks the run
  `stuck` with the reason, rather than continuing.

A cost ceiling is the one people actually want, and it is harder: token accounting per
run has to survive retries and cross the agent boundary. The task ceiling is a crude
proxy available now; the cost ceiling should follow rather than block it.

**Every limit is declared, and every refusal names it.** A run stopped by a ceiling
that does not say which ceiling is indistinguishable from a bug.

### Triggers

All three want the same thing — a launch identity that is not a person, which
`launched_by` already accommodates.

- **Cron.** A schedule attached to a workflow, launching with a fixed input. Reuses the
  existing scheduler; the input is a stored literal because a schedule cannot prompt.
- **Webhook.** An inbound endpoint mapping a payload to a run input via a pointer, the
  same JSON-Pointer vocabulary bindings and gates already use. This is what closes the
  gitops loop.
- **`launch_workflow` tool.** So the cortex can invoke a procedure rather than
  reconstruct it. Bounded by the same filing depth and fan-out caps that already stop a
  worker filing cards without limit — an agent that can launch a workflow that launches
  a workflow needs the same recursion guard `MAX_FILING_DEPTH` provides.

**A webhook is an unauthenticated inbound trigger for arbitrary pipeline execution.**
It must not ship before #1, and it needs its own shared secret regardless. Noted here
rather than left for whoever builds it to discover.

## Build order

1. **Fan-out width cap.** Smallest, and the only item on this list that is a live
   hazard rather than a missing capability.
2. **Run state**, plus `finished_at`, cancel, and delete. Everything else reports
   through it.
3. **Stuck detection** and notification on terminal transition.
4. **Run task ceiling**, parking the run `stuck` with its reason.
5. **`launch_workflow` tool** — highest value per unit of work, since it makes existing
   pipelines reachable by the autonomous path.
6. **Cron trigger.**
7. **Webhook**, gated on authentication existing.

## Risks

- **Stuck detection that is wrong in either direction.** A false positive parks a
  healthy run and trains people to ignore the signal; a false negative is the silence
  we have now. The detector must consider gates that are still pollable, not only tasks.
- **Cancellation racing a claim.** Cancelling a task another agent is claiming is the
  same race `claim_next_ready` already solves with a conditional update; use that
  pattern rather than inventing another.
- **Ceilings that truncate instead of refusing.** A partial fan-out feeding a fan-in
  that reports a subset as complete is a wrong answer delivered confidently, which is
  worse than a stopped run.
