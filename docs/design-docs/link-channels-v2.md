# Link Channels v2: Mirrored Conversations

## Problem

The current link system has two fundamental issues:

1. **Sender-side channels don't get created reliably.** When agent A messages agent B, only B's channel (`link:B:A`) is created on-demand. A's side (`link:A:B`) either doesn't exist or requires hacks (echo messages, bootstrap injection) to materialize.

2. **Routing is overengineered.** Envelopes, conclusion routing, originating channel tracking, metadata propagation — all of it exists to solve one problem: getting results back to the caller. But this complexity is the source of every bug. Messages get lost, conclusions fire prematurely, metadata doesn't propagate through hops, and LLMs don't reliably call `conclude_link`.

The fix isn't more routing logic. It's removing routing logic entirely and making the messaging layer do one thing well: mirror messages between two channels.

## Model: Two Phones on a Call

When two people text each other, both sides have their own chat history. I type a message, it appears on my screen as sent, and it appears on your screen as received. The carrier in the middle is invisible.

Agent link channels work the same way:

- Agent A has `link:A:B` — their side of the conversation with B
- Agent B has `link:B:A` — their side of the conversation with A
- When A sends a message on `link:A:B`, the system mirrors it to `link:B:A`
- When B replies on `link:B:A`, the system mirrors it to `link:A:B`

Both channels always have the full conversation. Both agents have their own independent Channel instance with their own context, tools, and LLM. The system is just the carrier.

## How It Works

### Channel Creation

Both link channels are created at startup for every link in config. No on-demand creation, no materialization hacks. If a link exists in config, both channels exist.

```
[[links]]
from = "chief-ai-officer"
to = "spacebot-tech-lead"
```

Creates:
- `link:chief-ai-officer:spacebot-tech-lead` (owned by chief-ai-officer)
- `link:spacebot-tech-lead:chief-ai-officer` (owned by spacebot-tech-lead)

The Channel runtime instance (LLM event loop) is still spawned on first message. But the channel record exists from boot.

### Sending a Message

Agent A calls `send_agent_message(target: "B", message: "hello")`.

The tool does two things:
1. Injects a **user message** into `link:B:A` (B's side) — this is B receiving A's message
2. Injects a **self message** into `link:A:B` (A's side) — this is A's sent message appearing in their own history

The message on B's side is a normal `InboundMessage` with `source: "internal"` that triggers B's Channel to process it (run LLM, potentially reply).

The message on A's side is recorded in history but does NOT trigger LLM processing. It's just a log of what A sent, like "sent" receipts in a chat app.

### Receiving a Reply

When B's LLM responds on `link:B:A`, the `reply` tool fires. The outbound handler in main.rs detects this is an internal channel and mirrors the reply:

1. B's reply text is recorded on `link:B:A` (B's side) as a bot message — already handled by the reply tool
2. The system injects B's reply into `link:A:B` (A's side) as a user message — this triggers A's Channel to process it

A's Channel on `link:A:B` now has the conversation:
```
[A - sent]: hello
[B - received]: hey, what's up?
```

A's LLM runs and can reply, forward the info to another channel, or do whatever it decides.

### Concluding a Link Conversation

`conclude_link` is a final reply that stops the loop. When an agent determines the conversation objective is met, it calls `conclude_link(summary: "...")`. This:

1. Sends the summary as a final message to the peer (via the normal mirror mechanism)
2. Marks this side of the link as concluded — stops accepting further messages
3. The peer receives the conclusion, processes it, and can also conclude their side

`conclude_link` has **zero routing logic**. It doesn't know or care where the result ultimately needs to go. It just ends the conversation on this link channel.

### Bridging: Getting Results Back to the Originating Channel

When an agent's link channel concludes (receives a conclusion from the peer or calls `conclude_link` itself), the **originating channel** — the channel that called `send_agent_message` to start this link conversation — needs to hear about it.

`send_agent_message` records `initiated_from: Option<ChannelId>` on the link Channel when it's first used. This is the channel that triggered the delegation.

When the link channel concludes (either by calling `conclude_link` or by receiving a peer's conclusion), the system injects a **system retrigger message** into the `initiated_from` channel with the conclusion summary. This wakes up the originating channel's LLM, which sees "your delegation to X returned: [result]" and can relay it to the user or act on it.

Each link channel independently tracks its own `initiated_from`. No global chain. No metadata propagation through hops.

### Multi-Hop Example

User → chief-ai-officer → spacebot-tech-lead → community-manager → back up the chain.

**Step 1:** User messages chief-ai-officer on `portal:chat:chief-ai-officer`.

**Step 2:** chief-ai-officer calls `send_agent_message(target: "spacebot-tech-lead", message: "forward this to community manager, have them pick a random word and send it back")`. The tool returns a confirmation, the LLM's turn continues, and it replies to the user: "Sent the request to Spacebot Tech Lead, I'll let you know when I hear back."

- `link:spacebot-tech-lead:chief-ai-officer` gets the message → triggers spacebot-tech-lead's LLM
- `link:chief-ai-officer:spacebot-tech-lead` gets a sent record (no LLM trigger)
- `link:chief-ai-officer:spacebot-tech-lead` records `initiated_from = "portal:chat:chief-ai-officer"`

**Step 3:** spacebot-tech-lead reads the request, calls `send_agent_message(target: "community-manager", message: "pick a random word and reply with it")` from the link channel.

- `link:community-manager:spacebot-tech-lead` gets the message → triggers community-manager's LLM
- `link:spacebot-tech-lead:community-manager` gets a sent record
- `link:spacebot-tech-lead:community-manager` records `initiated_from = "link:spacebot-tech-lead:chief-ai-officer"`

**Step 4:** community-manager picks "nebula", calls `conclude_link(summary: "nebula")`.

- The summary is mirrored to `link:spacebot-tech-lead:community-manager`
- `link:community-manager:spacebot-tech-lead` is marked concluded

**Step 5:** spacebot-tech-lead receives the conclusion on `link:spacebot-tech-lead:community-manager`. The LLM runs and has a natural exchange on the chief-ai-officer link:

> **spacebot-tech-lead** (on link with chief-ai-officer): "Got the word back from community manager — they picked 'nebula'. Passing it along now."
>
> **spacebot-tech-lead** calls `conclude_link(summary: "The word was nebula")`

- The summary is mirrored to `link:chief-ai-officer:spacebot-tech-lead`
- `initiated_from` is `"link:spacebot-tech-lead:chief-ai-officer"` — retrigger hits the chief link
- `link:spacebot-tech-lead:community-manager` is marked concluded

**Step 6:** chief-ai-officer receives the conclusion on `link:chief-ai-officer:spacebot-tech-lead`.

- `initiated_from` is `"portal:chat:chief-ai-officer"` — retrigger hits the portal channel
- chief-ai-officer's LLM runs on the portal channel, sees the result, and replies to the user naturally: "The chain came back — community manager picked 'nebula'."

Every hop is independent. Each link channel knows only one thing: where it was initiated from. The chain bubbles up naturally through retriggers.

## What Gets Deleted

- `AgentEnvelope` and all envelope code (already stashed)
- `route_link_conclusion()` and all its routing logic
- `originating_channel`, `originating_source` fields on Channel
- `reply_to_agent`, `reply_to_channel` metadata propagation
- `original_sent_message` history seeding hack
- The complex outbound reply-routing block in main.rs that constructs reply messages with propagated metadata

## What Gets Added/Changed

### send_agent_message

Simplified. Two message injections:
1. Receiver side: normal `InboundMessage` to trigger the peer's LLM
2. Sender side: history-only record (marked so it doesn't trigger LLM)

Records `initiated_from` on the sender-side link Channel.

### conclude_link

Simplified to just a stop signal:
1. Mirrors the summary to the peer as a final message
2. Marks the channel as concluded
3. No routing logic

### main.rs outbound handler

For internal channels, the outbound handler becomes a simple mirror:
1. Extract sender agent ID from the channel
2. Look up the link to find the peer agent
3. Inject the reply into the peer's link channel as a user message

No metadata propagation. No originating channel forwarding. Just mirror.

### Link channel conclusion handling

When a link channel concludes (receives a peer's conclusion or calls conclude_link):
1. Mark `link_concluded = true`
2. If `initiated_from` is set, inject a system retrigger into that channel with the conclusion summary
3. The retrigger wakes up the originating channel so it can relay results

### Channel startup

Both link channels are pre-registered in `ChannelStore` at startup for every link in config. The runtime Channel instance is still spawned on first message.

### Prompts

`link_context.md.j2` updated — frame as a conversation channel. Instruct to use `conclude_link` when the objective is met to send a final summary and end the conversation.

## Phases

### Phase 1: Pre-create Both Link Channels

- Register both channel records in `ChannelStore` at startup
- Channel runtime instances still spawn on first message

### Phase 2: Mirror-Based Reply Routing

- Rewrite the outbound handler for internal channels as a simple mirror
- Remove all metadata propagation (`reply_to_agent`, `reply_to_channel`, `originating_channel`, `originating_source`)
- `send_agent_message` injects into both sides (receiver triggers LLM, sender is history-only)
- `send_agent_message` records `initiated_from` on the link Channel

### Phase 3: Simplify conclude_link

- Remove all routing logic from `conclude_link` / `route_link_conclusion`
- `conclude_link` becomes: mirror summary to peer + mark concluded
- On conclusion received: mark concluded + retrigger `initiated_from` channel
- Remove `originating_channel`/`originating_source` fields from Channel
- Remove envelope code

### Phase 4: Prompt Cleanup

- Update `link_context.md.j2` — conversation framing, conclude when done
- Remove envelope references from tool descriptions
- Verify org_context still makes sense

## Open Questions

1. **Persistent vs ephemeral link channels.** Link channels accumulate history. The compactor already handles any channel, so compaction should work. But do we want to reset link channel history between delegations? A fresh context per delegation might produce better results than an ever-growing conversation.

2. **Multiple active delegations.** If chief-ai-officer delegates to both spacebot-tech-lead AND spacedrive-tech-lead from the same portal channel, both results need to bridge back. Each link channel has its own `initiated_from`, so both retriggers hit the portal channel independently.

3. **Turn safety cap.** The current 20-turn cap prevents infinite loops. With `conclude_link` as a stop signal, this is still useful as a backstop. Keep it but maybe increase it — 20 turns for a real multi-turn exchange is pretty tight.
