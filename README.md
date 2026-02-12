<p align="center">
  <img src=".github/Ball.png" alt="Spacebot" width="120" height="120" />
</p>

<h1 align="center">Spacebot</h1>

<p align="center">
  <strong>An agentic system where every LLM process has a dedicated role.</strong><br/>
  Channels talk. Branches think. Workers execute.<br/>
  Nothing ever blocks.
</p>

<p align="center">
  <a href="https://www.gnu.org/licenses/agpl-3.0">
    <img src="https://img.shields.io/static/v1?label=License&message=AGPL%20v3&color=000" />
  </a>
  <a href="https://github.com/jamiepine/spacebot">
    <img src="https://img.shields.io/static/v1?label=Core&message=Rust&color=DEA584" />
  </a>
  <a href="https://discord.gg/gTaF2Z44f5">
    <img src="https://img.shields.io/discord/949090953497567312?label=Discord&color=5865F2" />
  </a>
</p>

<p align="center">
  <a href="#how-it-works">How It Works</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#tech-stack">Tech Stack</a> •
  <a href="docs/">Docs</a>
</p>

---

## The Problem

Most AI agent frameworks run everything in a single session. One LLM thread handles conversation, thinking, tool execution, memory retrieval, and context compaction — all in one loop. When it's doing work, it can't talk to you. When it's compacting, it goes dark. When it retrieves memories, raw results pollute the context with noise.

[OpenClaw](https://github.com/anomalyco/openclaw) _does_ have subagents, but handles them poorly and there's no enforcement to their use. The session is the bottleneck for everything.

Spacebot splits the monolith into specialized processes that only do one thing, and delegate everything else.

---

## How It Works

Five process types. Each does one job.

### Channels

The user-facing LLM process. One per conversation (Telegram DM, Discord thread, etc). Has soul, identity, and personality. Talks to the user. Delegates everything else.

A channel does **not**: execute tasks directly, search memories itself, or do any heavy tool work. It is always responsive — never blocked by work, never frozen by compaction.

When it needs to think, it branches. When it needs work done, it spawns a worker.

### Branches

A fork of the channel's context that goes off to think. Has the channel's full conversation history — same context, same memories, same understanding. Operates independently. The channel never sees the working, only the conclusion.

```
User A: "what do you know about X?"
    → Channel branches (branch-1)

User B: "hey, how's it going?"
    → Channel responds directly: "Going well! Working on something for A."

Branch-1 resolves: "Here's what I found about X: [curated memories]"
    → Channel sees the branch result on its next turn
    → Channel responds to User A with the findings
```

Multiple branches run concurrently. First done, first incorporated. Each branch forks from the channel's context at creation time, like a git branch.

### Workers

Independent processes that do jobs. Get a specific task, a focused system prompt, and task-appropriate tools. No channel context, no soul, no personality.

**Fire-and-forget** — do a job and return a result. Memory recall, summarization, one-shot tasks.

**Interactive** — long-running, accept follow-up input from the channel. Coding sessions, multi-step tasks.

```
User: "refactor the auth module"
    → Branch spawns interactive coding worker
    → Branch returns: "Started a coding session for the auth refactor"

User: "actually, update the tests too"
    → Channel routes message to active worker
    → Worker receives follow-up, continues with its existing context
```

Workers are pluggable. A worker can be a Rig agent with shell/file tools, an [OpenCode](https://opencode.ai) subprocess, or any external process that accepts a task and reports status.

### The Compactor

Not an LLM process. A programmatic monitor per channel that watches context size and triggers compaction before the channel fills up.

| Threshold | Action |
|-----------|--------|
| **>80%** | Background compaction (summarize oldest 30%) |
| **>85%** | Aggressive compaction (summarize oldest 50%) |
| **>95%** | Emergency truncation (hard drop, no LLM) |

Compaction workers run alongside the channel without blocking it. Summaries stack chronologically at the top of the context window.

### The Cortex

System-level observer. Generates a **memory bulletin** — a periodically refreshed, LLM-curated summary of the agent's knowledge injected into every channel's system prompt. Queries across multiple dimensions (identity, events, decisions, preferences), synthesizes into a ~500 word briefing. Replaces the static MEMORY.md approach with a dynamic, structured alternative.

---

## Architecture

```
User sends message
    → Channel receives it
        → Branches to think (has channel's context)
            → Branch recalls memories, decides what to do
            → Branch might spawn a worker for heavy tasks
            → Branch returns conclusion
        → Branch deleted
    → Channel responds to user

Channel context hits 80%
    → Compactor notices
        → Spins off a compaction worker
            → Worker summarizes old context + extracts memories
            → Compacted summary swaps in
    → Channel never interrupted
```

### What Each Process Gets

| Process   | Type         | Tools                                     | Context                             |
| --------- | ------------ | ----------------------------------------- | ----------------------------------- |
| Channel   | LLM          | Reply, branch, spawn workers, route       | Conversation + compaction summaries |
| Branch    | LLM          | Memory recall, memory save, spawn workers | Fork of channel's context           |
| Worker    | Pluggable    | Shell, file, exec, browser (configurable) | Fresh prompt + task description     |
| Compactor | Programmatic | Monitor context, trigger workers          | N/A                                 |
| Cortex    | LLM          | Memory recall, memory save                | Fresh per bulletin run              |

### Memory System

Memories are structured objects, not files. Every memory is a row in SQLite with typed metadata and graph connections, paired with a vector embedding in LanceDB.

- **Six types** — Fact, Preference, Decision, Identity, Event, Observation
- **Graph edges** — RelatedTo, Updates, Contradicts, CausedBy, PartOf
- **Hybrid search** — Vector similarity + full-text search, merged via Reciprocal Rank Fusion
- **Three creation paths** — Branch-initiated, compactor-initiated, cortex-initiated
- **Importance scoring** — Access frequency, recency, graph centrality. Identity memories exempt from decay.

### Heartbeats

Scheduled recurring tasks. Each heartbeat gets a fresh short-lived channel with full branching and worker capabilities.

- Multiple heartbeats run independently at different intervals
- Stored in the database, created via config, conversation, or programmatically
- Circuit breaker auto-disables after 3 consecutive failures
- Active hours support with midnight wrapping

### Multi-Agent

Each agent is an independent entity with its own workspace, databases, identity files, cortex, and messaging bindings. All agents share one binary, one tokio runtime, and one set of API keys.

---

## Quick Start

### Prerequisites

- **Rust** 1.85+ ([rustup](https://rustup.rs/))
- An LLM API key (OpenRouter, Anthropic, OpenAI, etc.)

### Build and Run

```bash
git clone https://github.com/jamiepine/spacebot
cd spacebot
cargo build --release
```

### Minimal Config

Create `config.toml`:

```toml
[providers.openrouter]
api_key = "env:OPENROUTER_API_KEY"

[defaults.routing]
default_model = "anthropic/claude-sonnet-4"
worker_model = "anthropic/claude-sonnet-4"

[agents.my-agent]

[messaging.discord]
bot_token = "env:DISCORD_BOT_TOKEN"

[[bindings]]
platform = "discord"
channel_id = "your-discord-channel-id"
agent = "my-agent"
```

```bash
cargo run --release
```

The binary creates all databases and directories automatically on first run. See the [quickstart guide](docs/quickstart.md) for more detail.

---

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Language | **Rust** (edition 2024) |
| Async runtime | **Tokio** |
| LLM framework | **[Rig](https://github.com/0xPlaygrounds/rig)** v0.30 — agentic loop, tool execution, hooks |
| Relational data | **SQLite** (sqlx) — conversations, memory graph, heartbeats |
| Vector + FTS | **[LanceDB](https://lancedb.github.io/lancedb/)** — embeddings (HNSW), full-text (Tantivy), hybrid search (RRF) |
| Key-value | **[redb](https://github.com/cberner/redb)** — settings, encrypted secrets |
| Embeddings | **FastEmbed** — local embedding generation |
| Crypto | **AES-256-GCM** — secret encryption at rest |
| Discord | **Serenity** — gateway, cache, events |
| Browser | **Chromiumoxide** — headless Chrome via CDP |
| CLI | **Clap** — command line interface |

No server dependencies. Single binary. All data lives in embedded databases in a local directory.

---

## Project Structure

```
spacebot/
├── src/
│   ├── main.rs              # CLI entry, config loading, startup
│   ├── lib.rs               # Re-exports, shared types
│   ├── config.rs            # Configuration loading/validation
│   ├── error.rs             # Top-level error types
│   ├── agent/               # Channel, branch, worker, compactor, cortex
│   ├── hooks/               # PromptHook implementations
│   ├── tools/               # All LLM tools (reply, branch, memory, shell, etc.)
│   ├── memory/              # Store, types, search, LanceDB, embeddings, maintenance
│   ├── llm/                 # Model routing, provider clients, SpacebotModel
│   ├── messaging/           # Discord, Telegram, webhook adapters
│   ├── conversation/        # History persistence, context assembly
│   ├── heartbeat/           # Scheduler, store
│   ├── identity/            # SOUL.md, IDENTITY.md, USER.md loading
│   ├── opencode/            # OpenCode subprocess worker integration
│   ├── secrets/             # Encrypted credential storage (redb)
│   ├── settings/            # Key-value settings (redb)
│   └── db/                  # SQLite connection setup, migrations
├── prompts/                 # System prompts (markdown files, not Rust strings)
├── migrations/              # SQLite migrations
├── docs/                    # Architecture and design documentation
└── Cargo.toml
```

---

## Documentation

| Doc | Description |
|-----|-------------|
| [Quick Start](docs/quickstart.md) | Setup, config, first run |
| [Config Reference](docs/config.md) | Full `config.toml` reference |
| [Agents](docs/agents.md) | Multi-agent setup and isolation |
| [Memory](docs/memory.md) | Memory system design |
| [Tools](docs/tools.md) | All available LLM tools |
| [Compaction](docs/compaction.md) | Context window management |
| [Cortex](docs/cortex.md) | Memory bulletin and system observation |
| [Heartbeats](docs/heartbeats.md) | Scheduled recurring tasks |
| [Routing](docs/routing.md) | Model routing and fallback chains |
| [Messaging](docs/messaging.md) | Adapter architecture (Discord, Telegram, webhook) |
| [Discord Setup](docs/discord-setup.md) | Discord bot setup guide |
| [Browser](docs/browser.md) | Headless Chrome for workers |
| [OpenCode](docs/opencode.md) | OpenCode as a worker backend |
| [Philosophy](docs/philosophy.md) | Why Rust |

---

## Status

Active development. Core architecture is implemented and running. See the [roadmap](docs/roadmap.md) for current progress.

---

## Contributing

Contributions welcome. Read [RUST_STYLE_GUIDE.md](RUST_STYLE_GUIDE.md) before writing any code, and [AGENTS.md](AGENTS.md) for the full implementation guide.

1. Fork the repo
2. Create a feature branch
3. Make your changes
4. Submit a PR

---

## License

AGPL-3.0 — see [LICENSE](LICENSE) for details.
