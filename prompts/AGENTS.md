# prompts/

Jinja2 prompt templates for all LLM process types.

## Structure

```
prompts/en/
├── channel.md.j2
├── branch.md.j2
├── worker.md.j2
├── compactor.md.j2
├── cortex.md.j2
├── cortex_bulletin.md.j2
├── cortex_chat.md.j2
├── cortex_profile.md.j2
├── factory.md.j2
├── ingestion.md.j2
├── memory_persistence.md.j2
├── tools/          (41 files)
├── fragments/      (8 files)
├── fragments/system/  (11 files)
└── adapters/       (2 files)
```

73 `.md.j2` files total.

## Tool Prompts (`tools/`)

One description template per tool: `reply_description.md.j2`, `shell_description.md.j2`, etc. Rendered and injected into process system prompts at startup.

## Fragments

Reusable snippets injected into multiple prompts.

**System fragments** (`fragments/system/`): tool syntax correction, worker overflow handling, truncation notices, etc.

**Context fragments** (`fragments/`): skills, projects, org context, conversation context.

## Adapter Prompts (`adapters/`)

- `email.md.j2` — email adapter context
- `cron.md.j2` — cron job execution context

## Rendering

`PromptEngine` in `src/prompts/engine.rs` renders templates with context variables via Jinja2. Templates receive typed context structs, not raw strings.

## Rules

- **Never** store prompts as string constants in Rust. All prompt text lives here.
- Edit templates here, not in Rust source.
- i18n-ready: `en/` directory structure supports additional language directories. Only English exists currently.
