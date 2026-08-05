# Human Decisions

A pipeline can stop and wait for a person. It cannot ask one a question and use the
answer. Task #30.

## Problem

`BlockKind::NeedsInput` parks a task for a human, and `POST /tasks/{n}/unblock`
releases it. So "wait for a person" works.

But unblocking is a single undifferentiated act. The task resumes and **nothing
downstream learns what the person decided.** "Approve this deploy?" and "which of these
three options?" are not expressible, because there is no channel for the answer — only
for the fact that someone acted.

Today the workaround is an agent step that asks in a channel and reports what it heard.
That is slower, costs a model call, and can be wrong about what was said. It also
launders a human decision through a model, so the run record cannot distinguish "the
operator approved this" from "a model believed the operator approved this". For a
deploy gate that distinction is the entire point.

This pairs directly with branching (#19): a human decision is exactly the kind of value
a condition should route on. Approve → ship. Reject → roll back. Neither is reachable
while the answer has nowhere to live.

## The constraint

**A human must not be able to set an arbitrary task's outputs.** Decided earlier and
deliberately: the outputs of an agent task are a record of what that agent produced,
and a person editing them destroys the only honest account of what happened. Anything
built here has to respect that.

So this cannot be "let humans fill in outputs on a blocked task". That would be the
same feature with the provenance quietly removed.

## Design

### A decision step

A step kind — alongside `agent` and the proposed `command` — whose entire purpose is
the answer. Its `output_schema` is written by the template author and describes what is
being asked for; the person answering fills exactly that and nothing else.

The distinction survives into the record: a decision step's outputs are *known* to have
come from a person, because that is the only thing that kind of step can produce. No
mixing, no ambiguity, and no need to trust a field saying who wrote what.

```
kind          decision
prompt        the question, as the person will read it
output_schema what a valid answer looks like
asked_of      who may answer — a person, a group, or anyone
timeout       optional; what happens if nobody answers
```

The schema doing double duty is the point. A yes/no gate is
`{"approved": {"type": "boolean"}}`; a three-way choice is an `enum`; a free-text reason
is a `string`. The existing contract validation already enforces it at completion, so a
malformed answer is refused the same way a malformed agent output is — no second
validation path.

### It is a task, like everything else

A decision step compiles to a task, sits in the graph, has parents and children, binds
inputs, and produces outputs. It parks itself waiting for a person instead of being
claimed by a worker.

That means everything already built applies unchanged: the graph view shows it, gates
can hold it, a loop can contain it, branching can route on its answer, and a run's
`stuck` detector counts it as legitimately waiting rather than wedged — which is a
distinction the detector must actually make, since a decision waiting on a person is
not a stalled run.

### Timeouts

An unanswered decision is the common failure and needs an answer that is not "wait
forever". Options, declared per step:

- **wait** — default; it parks until answered, and the run is legitimately blocked.
- **default after N** — a declared default answer applies, recorded *as* a default so
  the record does not claim a person chose it.
- **fail after N** — the step fails and the failure path routes it.

The middle one is the one to get right. A defaulted answer that looks identical to a
human answer in the run record is the provenance problem returning through a side door.

### Asking

A decision needs to reach someone. The notification machinery already exists —
`TaskApproval` notifications, an action URL, the task drawer — so the first cut is a
notification pointing at the task, answered in the drawer.

Delivering the question into a chat channel and accepting the reply there is the
obvious follow-on, and it is where the provenance question gets genuinely hard: a
message in a channel is attributable to a person, but parsing an answer out of prose
puts a model back in the middle. Structured replies, or a link back to the drawer,
rather than free-text interpretation.

## Build order

1. The `decision` step kind, compiling to a task that parks for a person.
2. Answering in the task drawer, validated against the step's `output_schema`.
3. Notification on a decision becoming answerable.
4. `stuck` detection treating an unanswered decision as waiting, not wedged.
5. Timeouts, with a defaulted answer recorded as defaulted.
6. Answering from a channel — last, because provenance there is the hard part.

## Risks

- **Provenance erosion.** Every shortcut here — a default that looks like an answer, a
  model parsing a reply, an operator editing outputs "just this once" — removes the one
  property that makes a decision step worth having over an agent step that asks.
- **A decision inside a loop.** Each pass asks again, which may be right (re-approve
  each attempt) or maddening (three prompts for one deploy). The author should say
  which; defaulting silently to either will be wrong half the time.
- **Blocking a run indefinitely** is correct behaviour and looks identical to a bug on
  any dashboard that does not distinguish them. It has to be visibly *waiting on a
  person*, not merely not running.
