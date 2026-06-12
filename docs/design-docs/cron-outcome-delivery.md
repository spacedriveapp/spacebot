# Cron Outcome Delivery

Cron jobs with a delivery target currently spam it. A daily briefing that spawns three workers produces four Telegram messages: one when the channel starts talking, one per worker result as they arrive. The user gets fragments instead of a briefing.

The fix is structural. The cron channel's `reply` tool is replaced by a buffering mechanism. Replies accumulate silently during the run. When the channel exits, the buffer is flushed as a single delivery. The agent cannot accidentally deliver early — the runtime makes it impossible.

---

## Root Cause

A cron channel is a real channel. When the agent calls `reply`, the message routes to the delivery target immediately — the same path as any platform channel sending to Telegram. There is no gate. The cron scheduler's `await_cron_delivery_response()` takes the first response it sees, but by the time it does, Telegram has already received everything.

The system prompt tells the agent "no user present, execute autonomously" but says nothing about delivery discipline. The agent behaves naturally: it narrates as it works, sends status as workers complete, delivers a summary at the end. Four messages.

Prompt-only fixes are fragile. A user-modified cron prompt loses the discipline instruction. A future prompt change can reintroduce the problem silently. The runtime must enforce single delivery regardless of what the prompt says.

---

## Design

### The Reply Buffer

When a cron job has a delivery target, the channel is created with a `CronReplyBuffer` instead of a live `RoutedSender`. The buffer is an `Arc<Mutex<Vec<String>>>` — every `reply` call appends to it. Nothing is sent to the delivery adapter during the run.

```rust
pub struct CronReplyBuffer {
    entries: Arc<Mutex<Vec<String>>>,
}

impl CronReplyBuffer {
    pub fn stage(&self, content: String) {
        self.entries.lock().unwrap().push(content);
    }

    pub fn take_all(&self) -> Vec<String> {
        self.entries.lock().unwrap().drain(..).collect()
    }
}
```

The `reply` tool receives a `ReplyTarget` enum. For normal channels this is `ReplyTarget::Live(RoutedSender)`. For cron channels it is `ReplyTarget::Buffered(CronReplyBuffer)`. The tool impl branches on this:

```rust
match &self.target {
    ReplyTarget::Live(sender) => sender.send(content),
    ReplyTarget::Buffered(buffer) => buffer.stage(content),
}
```

The agent calls `reply` however many times it wants. No errors, no changed tool signature. The buffer silently absorbs every call.

### Delivery at Exit

After the channel's LLM loop exits, the cron runner calls `buffer.take_all()`. What it does with the entries depends on how many there are:

- **Zero entries** — the agent never called `reply`. Fall back to the last LLM text response from the run (captured separately via the existing `await_cron_delivery_response` mechanism). If that's also empty, deliver nothing.
- **One entry** — deliver it directly.
- **Multiple entries** — concatenate with double newlines and deliver as one message. No LLM synthesis step — the agent is responsible for structuring its final reply well. If it called `reply` three times with three chunks, those chunks are joined in order.

The concatenation-without-synthesis approach is intentional. Adding an LLM synthesis step here introduces latency, cost, and a failure point. The better solution is the system prompt (below) ensuring the agent only calls `reply` once with a complete message.

### System Prompt Addition

The cron system prompt gains a delivery section that makes the buffering behavior explicit:

```
## Delivery

Your reply() calls are buffered — nothing is sent to the delivery target
until your run completes. When you have finished all your work and have the
complete output ready, call reply() once with the full synthesized message.

Do not call reply() for interim progress or partial results. Workers report
back to you — collect their results, synthesize, then reply once.

If you have nothing meaningful to report, do not call reply() at all.
```

The structural enforcement makes this safe even if the user modifies the prompt and removes this section. The buffer still holds everything. Single delivery still happens. The prompt guidance is about quality (one well-structured message vs concatenated fragments), not correctness.

### No New Config, No UI Changes

There is no `output_mode` setting. Every cron channel with a delivery target uses the buffer. There is no opt-out in the UI — this is always the correct behavior for cron delivery.

The `CronFormData`, `CreateCronRequest`, and all related types are unchanged. The buffer is an implementation detail of the cron runner, invisible to the API surface.

The only observable behavioral change is: the delivery target receives one message instead of many, and it arrives when the run completes rather than progressively throughout.

### Cron Jobs Without a Delivery Target

Some cron jobs have no delivery target — they do background maintenance work with no output destination. These are unaffected. The buffer is only created when `delivery_target` is set. When it is not, the channel runs as today with the `reply` tool wired to a no-op (its current behavior for targetless cron jobs).

---

## What the Agent Experiences

From the agent's perspective, `reply` works exactly as documented. It accepts content, returns a confirmation. The difference is the confirmation message changes slightly:

- **Live mode:** `"Message sent."`
- **Buffered mode:** `"Message staged for delivery at end of run."`

This surfaces the buffering behavior in tool results, so if the agent calls reply multiple times it can see in its context that previous calls were staged, not delivered. This nudges it toward the intended pattern without preventing anything.

---

## Implementation Touchpoints

- `src/cron/scheduler.rs` — create `CronReplyBuffer` when `delivery_target` is set; pass to channel; call `buffer.take_all()` and deliver after run
- `src/tools/reply.rs` — add `ReplyTarget` enum; branch on live vs buffered in `call()`
- `src/agent/channel.rs` — accept `ReplyTarget` parameter in channel construction; thread through to reply tool registration
- `prompts/cron.md.j2` (or equivalent) — add delivery section as above

No schema changes. No API changes. No UI changes.
