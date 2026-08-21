# messaging/

Trait-based adapter system. 10 files. One trait, seven adapters, one manager.

## Trait Pattern (`traits.rs`)

`Messaging` trait: `name()`, `start()`, `respond()`, `send_status()`, `broadcast()`, `fetch_history()`, `health_check()`, `shutdown()`.

Static dispatch by default. `MessagingDyn` companion trait with blanket impl enables `Arc<dyn MessagingDyn>` for runtime polymorphism. Only use `MessagingDyn` when you need to store mixed adapter types.

## Adapters

| File | Library | Notes |
|------|---------|-------|
| `discord.rs` | serenity | Gateway, rich embeds, interactions, reactions |
| `slack.rs` | slack-morphism | Socket Mode, Block Kit, slash commands, streaming via message edits |
| `telegram.rs` | teloxide | Long-poll, media attachments, group + DM |
| `email.rs` | IMAP/SMTP | Largest adapter (1919 lines). Email search, thread tracking |
| `twitch.rs` | twitch-irc | Chat only, trigger prefix required |
| `webchat.rs` | SSE | Embeddable portal, per-agent session isolation, streaming |
| `webhook.rs` | axum | Programmatic access, HTTP receiver, no auth by default |

Each adapter is self-contained. Don't share state between adapters.

## MessagingManager (`manager.rs`)

Starts all configured adapters. Fans in `InboundMessage` from every adapter into a single `mpsc` channel. Routes `OutboundResponse` back to the originating adapter by name.

```
[discord] ──┐
[slack]   ──┤  mpsc::Sender<InboundMessage>  →  channel router
[telegram]──┤
[webhook] ──┘

channel router  →  mpsc::Sender<OutboundResponse>  →  adapter.respond()
```

Adapter failures are isolated. One adapter crashing doesn't take down the others.

## Message Coalescing

Rapid-fire messages from the same user are batched into a single LLM turn. Debounce window is configurable. Each coalesced batch includes timing context so the LLM knows messages arrived close together.

DM bypass: direct messages skip coalescing and go straight through. Only group/channel messages are batched.

## Adding an Adapter

1. Implement `Messaging` for your struct in a new file.
2. Register it in `MessagingManager::start_all()`.
3. No changes needed to the trait or manager fan-in logic.
