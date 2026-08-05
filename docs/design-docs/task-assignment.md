# Task Assignment

Say what a task needs, not who should do it. Task #28.

## Problem

Every task carries `assigned_agent_id`, set at creation or inherited from whoever
launched the run. A workflow step may name a different agent, but it must name *one* —
there is no way to say "whichever agent can do this".

The hard part is already built. `claim_next_ready` is a race-safe conditional UPDATE:
several agents can compete for the same work and exactly one wins. The mechanism for
pull already exists and is tested. What is missing is any notion of **what an agent can
do**, so there is nothing to match a task against, and the claim is therefore filtered
by name.

Three consequences:

- **A second agent does not spread load.** Work is addressed by name, so adding
  capacity means re-addressing work.
- **Specialisation is hard-coded.** "Run the Rust step on one agent and the design step
  on another" means naming both in the template, and re-editing every template when the
  fleet changes.
- **A busy or dead agent blocks its queue.** Its tasks wait for it specifically, even
  when another agent could do them. The reaper returns crashed work to `ready`, and
  `ready` still means ready *for that agent*.

## Design

### Capabilities are declared, not inferred

An agent declares what it can do — a set of labels. Not inferred from its tools, its
model, or its history: inference here is a guess that fails silently, and the failure
looks like work quietly not happening.

```
agent "main"     capabilities: [rust, typescript, review]
agent "designer" capabilities: [design, review]
```

Labels are opaque strings the operator chooses. Resisting a taxonomy is deliberate —
every scheme invented up front is wrong for the fleet that eventually exists, and an
opaque label can be renamed without a migration.

### A task requires, or names

`assigned_agent_id` stays and stays the default. Naming an agent is push, it works
today, and it must keep working — a fleet of one has no use for any of this, and that
is the common case.

Alongside it, a task may instead declare `requires`: a set of capabilities. Such a task
is **unassigned** and sits in a pool. Any agent whose capabilities cover the
requirement may claim it, and the existing conditional UPDATE decides who does, exactly
as it decides today.

A workflow step gains the same choice — name an agent, or state a requirement.

### The claim changes in one place

Today:

```sql
WHERE assigned_agent_id = ? AND status = 'ready'
```

Becomes: that, **or** unassigned with every required capability held by the claiming
agent. Claiming stamps `assigned_agent_id`, so a claimed task looks exactly like a
pushed one from that moment on — the attempt log, the reaper, and the failure budget
need no changes, and a reaped task returns to the pool it came from rather than to a
named agent that may be gone.

### Nothing capable

A task requiring capabilities no agent holds sits in the pool forever, which is the
"parked and silent" failure this codebase keeps rediscovering. It should be visible:
the ready sweep already reports `stalled` and `gated` holds, and this is a third —
*nothing in the fleet can do this*.

It is also knowable earlier. A workflow step requiring a capability no agent declares
can be refused at **launch**, the way an unknown step reference already is. That does
not cover an agent being deleted mid-run, which is what the sweep report is for.

## What this is not

Not scheduling, priority, or fairness. Not load balancing beyond "whoever asks first
and can do it". Not routing by cost or model. Those are all reasonable later and all
need a working capability model first — and each one added before it would have to be
rebuilt on top of one.

## Build order

1. Capabilities on agents, declared and surfaced.
2. `requires` on tasks, and the claim query accepting an unassigned match.
3. `requires` on workflow steps, with launch-time refusal when nothing can satisfy it.
4. The sweep reporting tasks nothing in the fleet can claim.
5. UI: capabilities in agent config, requirement in the step editor, the unclaimable
   hold shown on the board.

## Risks

- **A pool nobody watches.** An unassigned task that no agent can claim is invisible
  unless the sweep says so. Step 4 is not optional polish.
- **Capability drift.** Labels are free text, so `rust` and `Rust` are two capabilities
  and one of them matches nothing. Offer the existing set when authoring rather than
  validating a taxonomy into existence.
- **Reaped tasks losing their pool.** A claimed task is stamped with an agent; the
  reaper must return it to *unassigned* if that is where it came from, or a crashed
  agent takes the work with it — the exact failure this feature exists to prevent.
