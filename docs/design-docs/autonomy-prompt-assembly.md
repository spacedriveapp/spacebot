# Autonomy Prompt Assembly

The autonomy channel is a resident internal process. It waits while idle, receives ephemeral heartbeat messages, and keeps live worker controls across autonomy epochs. Its prompt separates standing system policy from the current heartbeat briefing.

## Prompt Layers

An autonomy turn has three layers.

### Standing Policy

`Channel::build_system_prompt()` renders the normal shared context providers, but `channel.md.j2` selects an autonomy-specific operating contract for `ChannelKind::Autonomy`.

The autonomy contract states:

- No human receives channel text.
- Work and outcomes happen through tools.
- `pending_approval` tasks cannot execute.
- Active work is tended before more work is admitted.
- A heartbeat does not require activity.
- `autonomy_complete` closes the current epoch, not the channel.
- Useful workers are never cancelled to make an epoch finish.

The autonomy path omits the user-channel communication and silence rules. It does not tell the process to reply, react, interpret mentions, or use `skip`.

The runtime also applies the autonomy level to tool registration. `observe` and `suggest` do not receive execution or worker-routing tools. `observe` receives no task mutation tools. `act` receives execution, worker spawn, and worker routing capabilities.

Identity, memory, skills, projects, working memory, goals, and live process status continue to use the normal channel context providers. They remain system context.

### Heartbeat Briefing

`build_run_briefing()` renders `autonomy_channel.md.j2` for the current heartbeat. The briefing contains:

- Trigger reason and elapsed epoch time.
- Newly claimed wake events.
- Recent completed epoch summaries.
- Current task state and prior attempts.
- Active goals.
- Every nonterminal worker, including retained interactive workers.
- Autonomy level and task approval boundaries.
- The per-epoch task budget.

The supervisor sends this briefing as a synthetic system message tagged with:

```text
autonomy_generation = <u64>
autonomy_epoch_start = <bool>
```

System messages are excluded from conversation persistence. The generation tag prevents a queued message from an older epoch from entering the current one.

An epoch-start heartbeat clears the previous epoch's live model history and contract counters. It keeps worker handles, worker input routes, consumed outcome versions, and active process status. Heartbeats inside the same epoch retain history so worker results and follow-up decisions share context.

### Worker Results

Worker and branch results use `retrigger_autonomy.md.j2`. The retrigger contract is intentionally narrower than a heartbeat:

- Reconcile the result first.
- Continue the intent that produced it.
- Write required task comments, status changes, or memories.
- Spawn or route only a direct follow-up.
- Complete the epoch when the selected work is settled.

A worker result is not a fresh board survey. It does not authorize unrelated work.

## Active-Worker Policy

The heartbeat renders every nonterminal worker, not only workers whose display status is `running`. This includes `created`, `running`, `waiting_for_input`, `cancelling`, `timing_out`, and `completing` lifecycles.

The decision order is:

1. Reconcile completed work.
2. Tend work that serves the current highest-priority task.
3. Wait when existing workers already cover the best available work.
4. Add work only when it is independent, approved, goal-aligned, and worth the capacity.
5. Record a no-action epoch when nothing needs attention.

This policy is prompt guidance backed by runtime ownership. Every branch and worker spawned by an epoch registers as a child before the epoch can finish. Routing follow-up input to a retained worker registers that worker in the current epoch again.

## Completion Contract

`autonomy_complete` stores the first accepted summary and action list on the generation-specific `AutonomyRunHandle`.

The tool rejects completion while registered children are active. Child admission and finish requests use the same mutex, so a spawn cannot pass a stale settling check. After children settle and pending retriggers drain, the channel marks the epoch quiescent. The supervisor commits the run row, publishes its summary once, clears that generation, and leaves the channel running.

If a model turn returns without completing and has no active children or pending results, the channel sends a bounded contract retry. Exhausting that retry budget records the fallback summary. This is a completion contract, not a wall-clock timeout.

## Transcript Rules

- Heartbeats are not persisted.
- Wake payloads and task surveys are not persisted.
- Worker retrigger scaffolding is not persisted.
- Current-epoch tool and assistant history remains live until the epoch closes.
- Completed epoch summaries are stored in `autonomy_runs` and published once to the autonomy timeline.
- The next epoch receives bounded recent summaries from the run store.

The run row is the idempotency boundary. Only the caller that changes a run from `running` to a terminal state may publish its outcome.

## Budgets

`max_turns` bounds one agentic model invocation. It does not bound the channel or epoch lifetime. `max_tasks_per_run` limits task breadth inside an epoch.

Provider calls, tools, and workers retain their own operation-level limits. Autonomy has no soft warning or hard run timeout. Heartbeats continue while an epoch owns work.

## Inspection

Prompt records must identify the process as `ChannelKind::Autonomy`. Each heartbeat carries the epoch generation in message metadata, while durable run ids remain in `autonomy_runs`, `worker_runs.run_id`, and `branch_runs.run_id`.

The portal should derive epoch dividers from `autonomy_runs`, not heartbeat message text. Wake provenance comes from `wake_event_ids` on the run row.
