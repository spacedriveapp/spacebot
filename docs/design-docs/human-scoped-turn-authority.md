# Human-Scoped Turn Authority

**Status:** Proposed  
**Scope:** Human permissions, per-turn context/tool policy, commands, delegation, approvals, and audit provenance  
**Supersedes:** The platform-ID `CommandAccess::Authority` model proposed in `slash-commands.md`

## Summary

Spacebot already knows which configured Human is speaking. Participant awareness resolves an inbound sender to a `HumanDef`, uses the Human ID as the canonical participant key, tracks their role and profile, and injects recent participant context into the channel prompt.

That identity currently changes what the model **knows**, but not what it **may do**.

Every admitted sender reaches the same channel process, sees substantially the same agent context, and can cause the model to use the same channel, branch, worker, memory, task, and system capabilities. `HumanDef.role` is descriptive text. It is not an authority boundary.

This design makes Human identity the root of turn authority:

```text
Human
  -> resolve effective policy
  -> freeze TurnAuthority for this turn
  -> filter context and tool schemas
  -> enforce the same policy at command/tool execution
  -> propagate an equal-or-narrower authority ceiling to descendants
  -> record Human + authority + action provenance
```

The desired product behavior is simple:

> A guest can talk to the same high-quality agent, but the turn they trigger is structurally incapable of reading private context or performing actions outside that guest's authority.

Tool hiding is part of this system, not the whole system. A safe guest turn must also exclude private memory and other sensitive context, protect deterministic commands, prevent delegation-based privilege escalation, and enforce permissions again when a tool actually executes.

---

## Why This Belongs on `HumanDef`

A platform account says where a message came from. A Human says who Spacebot believes is acting and what relationship that person has to the agent.

Spacebot already has the right product object:

```rust
pub struct HumanDef {
    pub id: String,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub bio: Option<String>,
    pub description: Option<String>,
    pub discord_id: Option<String>,
    pub telegram_id: Option<String>,
    pub slack_id: Option<String>,
    pub email: Option<String>,
}
```

The missing fields are authority, not another parallel user system.

The first implementation remains config-backed. A future database-backed Human registry can replace storage without changing the turn-authority contract, just as participant awareness already uses a stable `participant_key` boundary.

---

## Current System

### What already works

- `InboundMessage.sender_id` preserves the platform sender.
- `track_active_participant()` resolves the sender against configured Humans.
- A known sender gets the canonical `HumanDef.id` as their participant key.
- Unknown senders fall back to an adapter-scoped key.
- The channel tracks active participants, display names, roles, profiles, and last activity.
- Participant context is rendered into each turn with recent working-memory activity.
- Coalesced user messages retain visible speaker labels.
- Conversation history persists raw sender IDs and display names.

Relevant production paths:

- `src/config/types.rs` — `HumanDef`, `ParticipantContextConfig`
- `src/conversation/participants.rs` — Human resolution and active participants
- `src/agent/channel.rs` — participant tracking and prompt construction
- `src/memory/working.rs` — participant-context rendering
- `src/commands/registry.rs` — typed commands without access policy

### What does not exist

- Human roles do not grant or deny capabilities.
- Tool schemas are not filtered by the triggering Human.
- Tool execution does not re-check Human authority.
- Commands are not authorized by Human.
- Branches and workers do not inherit a Human authority ceiling.
- Long-term memory is global per agent and is injected without Human visibility policy.
- Task creation/control is not bound to an authenticated Human principal.
- Multi-Human coalescing has no safe authority rule.
- Programmatic senders can assert identity without producing a durable Human principal.
- There is no Human-action audit ledger.

---

## Design Goals

1. **Human-centric policy.** Permissions are attached to Humans and reusable roles, not scattered platform IDs.
2. **Least privilege per turn.** A turn receives only the context and tools its triggering Human may use.
3. **Defense in depth.** Hidden tools are also rejected at execution time.
4. **No privilege amplification.** Commands, branches, workers, retries, resumptions, hooks, and approvals cannot silently widen authority.
5. **Safe multi-user channels.** One participant cannot borrow another participant's authority through batching, shared history, task steering, or approvals.
6. **Explainability.** Denials and audit records can identify the Human, required capability, applicable policy, and attempted action.
7. **Storage independence.** The policy model works with today's config-backed Humans and a future DB-backed registry.
8. **Fail closed.** Missing identity, missing capability metadata, or failed policy resolution never creates authority.

## Non-Goals

- Redesigning platform-account linking or Human verification in this phase.
- Implementing organization-wide ABAC or an enterprise policy language.
- Asking the LLM to decide whether a Human is trusted.
- Treating prompt instructions as an authorization mechanism.
- Solving all memory ownership semantics in the first patch. This design defines the boundary memory must consume.

---

## Security Invariants

These are architectural requirements, not recommendations.

1. **Attribution is not authorization.** A display name, prompt label, or model belief never grants authority.
2. **The trigger principal controls the turn.** Other Humans merely present in the channel do not contribute authority.
3. **Effective authority is immutable for a turn.** Hot reload affects subsequent turns, not one already executing.
4. **Children cannot exceed parents.** Descendant authority is an intersection with the parent's authority ceiling.
5. **Unknown tools fail closed.** A tool without declared capability metadata is hidden and rejected outside explicitly trusted system contexts.
6. **Commands use the same policy system.** Deterministic control-plane execution is not an authorization bypass.
7. **Private context follows read capabilities.** Preventing writes while injecting private memory still leaks data.
8. **Approvals authorize operations, not people or turns.** One approval cannot convert a guest turn into an owner turn.
9. **Multiple principals never produce a union of permissions.** Mixed-Human turns are split; exceptional combined turns use intersection semantics.
10. **System work has explicit provenance.** Cron, cortex, API, hooks, and resumptions never masquerade as owner authority.

---

## Policy Model

### Capabilities, not raw tool names

Humans should receive semantic capabilities. Tools and commands declare the capabilities they require.

Examples:

```text
chat.respond
web.read
memory.read.shared
memory.read.own
memory.write.own
memory.manage.shared
tasks.read
tasks.create
tasks.manage.own
tasks.manage.all
agents.branch
agents.delegate.safe
agents.delegate.system
files.read.workspace
files.write.workspace
system.execute
network.request
messages.send
channels.configure
agent.configure
secrets.read
approvals.request
approvals.grant
```

This decouples policy from implementation details. Renaming `shell` to `terminal`, or splitting one tool into several, does not require rewriting every Human definition.

Capabilities are namespaced strings initially. A central registry validates known names during config load.

### Access levels and roles

`access_level` is a product-facing preset. `role` remains a descriptive or organizational label. Reusable policy roles provide defaults.

```rust
pub enum HumanAccessLevel {
    Owner,
    Trusted,
    Member,
    Guest,
    Blocked,
    Custom,
}

pub struct HumanPermissions {
    pub access_level: HumanAccessLevel,
    pub roles: Vec<String>,
    pub allow: CapabilitySet,
    pub deny: CapabilitySet,
}
```

Suggested semantics:

| Level | Intended use | Default posture |
|---|---|---|
| `owner` | Agent owner | Broad authority, still subject to confirmations and runtime containment |
| `trusted` | Close collaborator/operator | Productive tools, no agent/system administration unless explicitly granted |
| `member` | Normal known participant | Chat, bounded read tools, own tasks/memory |
| `guest` | Social/shared-channel participant | Chat and explicitly safe utilities only |
| `blocked` | Known but denied | No turn creation |
| `custom` | Fully declared | No implicit capabilities beyond configured roles/allow list |

Access levels are defaults, not magic checks spread through code. They resolve to ordinary capabilities.

### Config shape

```toml
[human_roles.guest]
allow = [
  "chat.respond",
  "web.read",
]
deny = [
  "memory.read.shared",
  "memory.manage.shared",
  "files.read.workspace",
  "files.write.workspace",
  "system.execute",
  "messages.send",
  "channels.configure",
  "agent.configure",
  "agents.delegate.system",
]

[human_roles.collaborator]
inherits = ["guest"]
allow = [
  "memory.read.own",
  "memory.write.own",
  "tasks.read",
  "tasks.create",
  "tasks.manage.own",
  "agents.branch",
  "agents.delegate.safe",
]

[[humans]]
id = "jack"
display_name = "Jack"
role = "friend"
access_level = "guest"
roles = ["guest"]

# Optional Human-specific deltas.
permissions_allow = ["media.generate"]
permissions_deny = ["network.request"]
```

`deny` always wins. Role inheritance must be acyclic and validated at startup/reload.

### Policy resolution

Effective authority is an intersection of ceilings with explicit deny precedence:

```text
instance ceiling
  INTERSECT agent ceiling
  INTERSECT channel/binding ceiling
  INTERSECT Human access-level + roles + overrides
  INTERSECT parent turn ceiling, if any
  INTERSECT task/worker grant, if any
  MINUS every applicable deny
```

A channel or worker can narrow Human authority. It cannot grant a capability the Human does not already have.

The policy resolver returns both the effective set and an explanation trace for denials and audit records.

---

## `TurnAuthority`

Every Human-triggered turn gets one immutable authority object.

```rust
pub struct TurnAuthority {
    pub turn_id: TurnId,
    pub principal: TurnPrincipal,
    pub capabilities: CapabilitySet,
    pub context_policy: ContextPolicy,
    pub authority_ceiling: CapabilitySet,
    pub policy_revision: String,
    pub origin: TurnOrigin,
}

pub enum TurnPrincipal {
    Human {
        human_id: String,
        participant_key: String,
    },
    UnknownParticipant {
        participant_key: String,
    },
    System {
        kind: SystemPrincipalKind,
    },
}

pub struct TurnOrigin {
    pub channel_id: String,
    pub source: String,
    pub adapter: String,
    pub sender_id: Option<String>,
    pub message_ids: Vec<String>,
}
```

The model may receive a concise rendering:

```text
Current Human: Jack (guest)
Authority: web.read, chat.respond
This turn cannot access private memory, files, system execution,
messaging, agent configuration, or privileged delegation.
```

That text improves model behavior, but enforcement comes from Rust.

### Unknown participants

Unknown participants receive an explicit configured fallback role, normally `guest` or `blocked`.

```toml
[human_access]
unknown = "blocked"
```

There is no implicit owner and no inference from display names, email-looking strings, or prompt content.

---

## Context Policy

A guest turn with no dangerous tools can still be unsafe if the prompt contains owner memories, private channel history, secrets, or privileged worker results.

`TurnAuthority` therefore includes a context policy:

```rust
pub struct ContextPolicy {
    pub memory_visibility: MemoryVisibility,
    pub history_visibility: HistoryVisibility,
    pub participant_visibility: ParticipantVisibility,
    pub project_context: ProjectContextVisibility,
    pub secret_context: SecretContextVisibility,
}
```

Initial policy behavior:

- `owner`: current behavior, subject to ordinary privacy controls.
- `trusted`: shared + explicitly granted Human/project context.
- `member`: shared context and own scoped memory only.
- `guest`: public/shared channel context only; no owner memory bulletin or private participant profiles.
- `blocked`: no turn.

The full system prompt must be assembled after `TurnAuthority` is resolved. Memory bulletin, high-importance memory, project context, participant profiles, skills, and history must each accept `ContextPolicy` rather than being injected globally and filtered afterward.

This is required for the claim that a guest turn is incapable of dangerous disclosure.

---

## Tool Surface

### Capability metadata

Every tool declares required capabilities.

```rust
pub struct ToolPolicy {
    pub required: &'static [&'static str],
    pub risk: ToolRisk,
    pub delegatable: bool,
}

pub enum ToolRisk {
    ReadOnly,
    Mutating,
    ExternalSideEffect,
    Privileged,
}
```

Examples:

```rust
ToolDef::new("web_search")
    .policy(ToolPolicy::requires(["web.read"]));

ToolDef::new("memory_save")
    .policy(ToolPolicy::requires(["memory.write.own"]));

ToolDef::new("terminal")
    .policy(ToolPolicy::requires(["system.execute"]));

ToolDef::new("send_message")
    .policy(ToolPolicy::requires(["messages.send"]));
```

Argument-level scope checks remain necessary. `files.read.workspace` cannot read arbitrary paths, and `tasks.manage.own` cannot mutate another Human's task merely because both use `task_update`.

### Two enforcement layers

#### 1. Tool-schema filtering

Before constructing the Rig agent for a turn, register only tools allowed by `TurnAuthority`.

Benefits:

- The model cannot plan around unavailable tools.
- Tool descriptions do not leak privileged capabilities.
- Prompt-injection attempts have less surface to target.
- Guest turns become cheaper and easier for the model to reason about.

#### 2. Execution-time guard

Every tool invocation passes through a single authority-aware dispatcher:

```rust
pub async fn execute_tool(
    authority: &TurnAuthority,
    tool: &ToolDef,
    args: serde_json::Value,
) -> ToolResult {
    authority.require_all(tool.policy.required)?;
    tool.validate_scope(authority, &args)?;
    tool.execute(args).await
}
```

This catches stale agent schemas, plugin/MCP mistakes, direct API invocation, confused-deputy bugs, and tools discovered after turn construction.

A denial is a typed result, never an invitation for the model to work around the restriction.

### Plugins and MCP

External tools are denied unless they declare capability metadata or are mapped by operator configuration.

```toml
[mcp.tool_capabilities]
"github.get_issue" = ["github.read"]
"github.create_issue" = ["github.write"]
```

An unknown MCP tool is not assumed read-only.

---

## Command Authority

Commands are tools on another execution plane and must use the same capabilities.

```rust
pub struct CommandDef {
    // existing fields...
    pub required_capabilities: &'static [&'static str],
}
```

Examples:

| Command | Required capability |
|---|---|
| `/help` | `chat.respond` |
| `/status` | `chat.respond` |
| `/tasks` | `tasks.read` |
| `/quiet` | `channels.configure` |
| `/active` | `channels.configure` |
| `/mention-only` | `channels.configure` |
| `/stop` | `tasks.manage.own` or `tasks.manage.all`, checked against active-turn ownership |
| `/model` | `agent.configure` |
| `/approve` | `approvals.grant` plus operation-specific policy |

This replaces the binary `Everyone` versus `Authority` proposal in `slash-commands.md`. The typed registry remains the correct mechanism; its access field becomes capabilities rather than a separate binding-level authority list.

Help output is filtered so each Human sees commands they can actually invoke. A denied command returns a concise reason without revealing sensitive state.

---

## Multi-Human Turns and Coalescing

Current coalescing may merge rapid messages from several Humans into one model turn. A privileged Human's authority must never cover another Human's text merely because their messages arrived together.

### Default rule

- Messages from the same Human may coalesce.
- A message from a different Human closes the current batch and starts another turn.
- Queue order remains arrival order.

The coalescing key becomes:

```text
channel_id + principal_key + authority_revision
```

### Exceptional combined turns

Some future collaborative workflow may intentionally construct a multi-Human turn. Its effective capabilities are the intersection of all contributing principals, never the union.

```text
combined authority = human_A capabilities INTERSECT human_B capabilities
```

The turn records every contributing Human and message ID.

### Steering an active turn

A follow-up message may steer or interrupt a running turn only when the sender has the required relationship to that turn:

- originating Human: may steer/cancel their own turn;
- Human with `tasks.manage.all`: may supervise another Human's turn;
- everyone else: starts or queues a separate turn.

Authorization, task ownership, and conversation participation remain separate concepts.

---

## Branches, Workers, and Delegation

Every descendant receives an authority ceiling derived from its parent.

```rust
pub struct SpawnContext {
    pub parent_turn_id: TurnId,
    pub principal: TurnPrincipal,
    pub authority_ceiling: CapabilitySet,
    pub requested_capabilities: CapabilitySet,
}

let child_capabilities = parent.authority_ceiling
    .intersection(&worker_definition.capabilities)
    .intersection(&requested_capabilities);
```

Required behavior:

- A guest cannot ask a branch to invoke tools hidden from the channel turn.
- A safe worker cannot spawn a privileged worker.
- OpenCode/external subprocess workers receive only an explicitly permitted environment, workspace, credentials, and command surface.
- Retries and resumptions reload the stored authority snapshot or re-authorize explicitly; they never default to owner.
- Worker completion events retain the initiating Human and parent turn provenance.
- System-created workers use a named system principal with an explicit policy, not an implicit superuser.

Delegation is a privilege boundary, not an escape hatch.

---

## Approvals

Approval is useful when a restricted Human can propose an operation that requires owner authority. It must not widen the guest's whole turn.

```text
guest turn proposes exact operation
  -> dispatcher rejects as approval-eligible
  -> ApprovalRequest stores tool + canonical args + principal + turn
  -> authorized Human approves that exact digest
  -> one operation executes under ApprovalGrant
  -> grant is consumed
```

```rust
pub struct ApprovalGrant {
    pub request_id: String,
    pub operation_digest: String,
    pub requested_by: TurnPrincipal,
    pub approved_by_human_id: String,
    pub granted_capabilities: CapabilitySet,
    pub expires_at: DateTime<Utc>,
    pub single_use: bool,
}
```

Rules:

- The approver needs `approvals.grant` and the capability required by the operation.
- The grant is bound to canonical tool arguments and operation digest.
- Argument changes invalidate approval.
- Approval does not expose the tool to the original turn for unrelated calls.
- Approval never persists as a Human-role change.
- Shared-channel approvals identify both requester and approver.

---

## Memory, Tasks, and Ownership

This design does not finish user-scoped memory, but it defines the identity and authority it must consume.

### Memory

Memory operations need both capability and visibility checks:

```text
memory.read.own
memory.read.shared
memory.read.all
memory.write.own
memory.manage.shared
```

The LLM must not choose an arbitrary `human_id` argument to escape scope. The dispatcher derives permitted Human scope from `TurnAuthority` and validates any requested target.

### Tasks

Tasks and workers should record:

- originating Human;
- current owner Human, if any;
- supervising agent;
- authority ceiling;
- Humans allowed to steer, cancel, approve, or receive results.

`tasks.manage.own` means the authenticated principal's tasks, not a free-form owner string supplied by the model.

---

## System Principals

Not every turn is initiated by a Human. System work requires explicit principals:

```rust
pub enum SystemPrincipalKind {
    CronJob { job_id: String },
    Cortex,
    Compactor,
    WorkerContinuation { worker_id: String },
    ApiClient { client_id: String },
    WebhookRoute { route_id: String },
    InternalMaintenance,
}
```

Each system principal has a configured capability ceiling. `System` does not mean unrestricted.

Cron and webhook definitions must declare their capabilities when created. A cron created by a Human cannot exceed the creator's authority unless an authorized approver grants a separate durable automation policy.

---

## Hot Reload and Policy Revisions

Human and role policy remains hot-reloadable, but a turn freezes the resolved authority at start.

Each policy snapshot receives a deterministic revision hash. `TurnAuthority.policy_revision` is stored with the turn and descendants.

On reload:

- new turns use the new policy;
- queued messages resolve at actual turn start;
- active turns keep their original authority unless the Human becomes `blocked` or a capability is emergency-revoked;
- emergency revocation cancels affected tool calls and descendants using a separate revocation channel.

This avoids half-old, half-new authority inside one model loop while still supporting urgent lockout.

---

## Audit Provenance

Every security-relevant action emits a structured event:

```rust
pub struct AuthorityAuditEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub turn_id: Option<TurnId>,
    pub principal: TurnPrincipal,
    pub channel_id: Option<String>,
    pub action: String,
    pub capability: Option<String>,
    pub target: Option<String>,
    pub decision: PolicyDecision,
    pub policy_revision: String,
    pub parent_turn_id: Option<TurnId>,
    pub approval_request_id: Option<String>,
}
```

Record at minimum:

- admission denial;
- command allow/deny;
- tool allow/deny;
- scope denial;
- child spawn and inherited ceiling;
- steering/cancellation attempts;
- approval request/grant/denial/consumption;
- emergency revocation.

Do not store secrets or complete sensitive tool arguments in the audit row. Store a safe target summary and canonical digest.

---

## Failure Behavior

| Failure | Required behavior |
|---|---|
| Human cannot be resolved | Apply configured unknown policy, normally blocked or guest |
| Human references missing role | Reject config reload; keep last known-good policy |
| Role cycle | Reject config reload |
| Unknown capability in Human config | Reject config reload with exact path |
| Tool has no policy metadata | Hide and deny outside trusted migration mode |
| Policy resolver fails | Deny turn/tool; emit audit event |
| Context filter fails | Do not construct the model turn |
| Descendant authority missing | Refuse spawn |
| Approval provenance missing | Refuse approval execution |
| Policy changes mid-turn | Continue frozen snapshot unless emergency-revoked |

No authorization failure should silently fall back to current unrestricted behavior.

---

## Implementation Plan

### Phase 1 — Policy types and config

1. Add `HumanAccessLevel`, `HumanPermissions`, role definitions, and capability validation.
2. Extend `HumanDef` with access level, roles, allow, and deny.
3. Add safe built-in presets and explicit unknown-Human behavior.
4. Produce a deterministic policy revision hash.
5. Keep current behavior only through an explicit migration preset for existing private installations.

### Phase 2 — Turn principal and same-Human batching

1. Resolve one `TurnPrincipal` before command or model dispatch.
2. Add immutable `TurnAuthority` to channel turn state.
3. Key coalescing by principal and policy revision.
4. Split batches on principal changes.
5. Persist principal/turn provenance in process-run events.

### Phase 3 — Tool filtering and hard enforcement

1. Add capability metadata to every built-in tool.
2. Filter tool registration per turn.
3. Add the central execution-time authority guard.
4. Add argument-level scope validation for file, memory, task, messaging, project, and worker tools.
5. Fail closed for MCP/plugin tools without mappings.

### Phase 4 — Commands

1. Add required capabilities to `CommandDef`.
2. Authorize before either Control or Agent dispatch.
3. Filter `/help` by authority.
4. Bind `/stop`, `/cancel`, `/approve`, and task controls to ownership/supervision rules.
5. Replace the platform-ID authority proposal in `slash-commands.md`.

### Phase 5 — Descendant authority

1. Add principal and authority ceiling to branch/worker spawn contexts.
2. Persist them with process runs.
3. Restrict external worker environment, credentials, workspace, and tools.
4. Make retries, resumes, retriggers, and completion handling preserve provenance.
5. Add explicit system-principal policies for cron, cortex, compactor, API, and webhook work.

### Phase 6 — Context isolation

1. Make prompt construction authority-aware.
2. Filter memory bulletin, high-importance memories, history, participant profiles, project context, and skills.
3. Integrate the user-scoped memory design with Human IDs and capability-derived scope.
4. Prevent cross-Human compaction and recall leakage.

### Phase 7 — Approvals and audit

1. Add operation-bound approval requests and single-use grants.
2. Add the authority audit ledger and Human/activity filtering.
3. Surface effective permissions, denials, active grants, and revocation in the API/UI.

---

## Test Matrix

Tests must prove negative guarantees, not only successful owner behavior.

### Policy resolution

- access-level defaults resolve correctly;
- role inheritance resolves correctly;
- deny overrides allow;
- role cycles and unknown capabilities reject reload;
- channel/agent ceilings can narrow but never widen Human authority;
- frozen turns do not change under ordinary hot reload.

### Turn construction

- known Human receives their policy;
- unknown Human receives configured fallback;
- display-name changes do not affect authority;
- guest prompt excludes owner/private context;
- tool schemas contain only permitted tools.

### Commands

- guest cannot run channel configuration commands;
- guest can run explicitly allowed read-only commands;
- denied commands never reach handlers;
- `/help` reflects effective authority;
- control-plane execution cannot bypass policy.

### Tools

- hidden tool calls are rejected by the dispatcher;
- direct/injected tool calls are rejected even if absent from model schema;
- path/task/memory/message scope is checked at execution;
- unknown plugin/MCP tools fail closed;
- model retries cannot recover a denied tool through another name.

### Delegation

- guest branch has no capabilities beyond the guest turn;
- guest cannot spawn a privileged worker;
- workers cannot widen their own capability request;
- external workers receive no unauthorized credentials or filesystem access;
- resumptions preserve authority;
- completion retriggers do not run as owner/system superuser.

### Multi-user behavior

- same-Human rapid messages coalesce;
- different-Human messages split into distinct turns;
- one Human cannot steer or cancel another's turn without supervision capability;
- combined-turn policy uses intersection, never union;
- privileged text and guest text are never executed under one privileged authority snapshot.

### Approvals

- only an authorized Human can grant;
- grant is bound to exact operation digest;
- changed arguments invalidate grant;
- grant is consumed once;
- approval does not widen the original turn;
- requester and approver provenance is retained.

### Acceptance scenarios

1. **Owner DM:** full configured tools, private context, confirmation rules intact.
2. **Guest DM:** normal conversation and safe web/media utilities; no private memory, files, shell, messaging, agent config, or privileged delegation.
3. **Mixed group:** owner and guest receive separate turn authority; rapid interleaving cannot borrow authority.
4. **Guest prompt injection:** requests to use hidden tools, delegate, alter configuration, read private memory, or impersonate owner fail structurally.
5. **Owner approval:** one exact guest-requested operation executes; subsequent guest calls remain restricted.
6. **Worker continuation:** a guest-created worker remains guest-bounded after restart and resume.
7. **Emergency revoke:** blocking a Human stops new turns and cancels privileged in-flight descendants according to policy.

---

## Open Decisions

1. Should existing installations migrate to an explicit `owner` Human automatically, or require operator confirmation?
2. Which capabilities belong in the first stable vocabulary versus remaining internal scopes?
3. Should `role` stay descriptive while `roles` carries reusable policy roles, or should the fields be unified?
4. Which safe utility tools should the built-in `guest` preset include?
5. How should channel history be partitioned when a guest enters an existing owner conversation?
6. Should owner approval execute the operation in an isolated approval worker rather than re-entering the guest's model turn?
7. Which emergency revocations cancel active work versus merely block the next tool call?
8. How should public/shared memories be explicitly marked so they are safe to inject into guest turns?

---

## Product Principle

The model should be equally intelligent for every Human. Authority changes its **reach**, not its quality.

A guest does not get a worse agent. They get an agent with a smaller, explicit world:

- only context they may know;
- only tools they may invoke;
- only tasks they may own or steer;
- only side effects they may request;
- no route to privilege through commands, batching, delegation, retries, or approvals.

That is the difference between asking an LLM to behave safely and constructing a system in which unsafe behavior is unavailable.
