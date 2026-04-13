import type {AgentConfigSection} from "./types";

export const SECTIONS: AgentConfigSection[] = [
	{
		id: "general",
		label: "General",
		group: "general",
		description: "Agent metadata",
		detail:
			"The agent's display name and role. The display name is shown in the UI and messaging platforms. The role describes the agent's purpose (e.g. Research Assistant, Code Reviewer).",
	},
	{
		id: "soul",
		label: "Soul",
		group: "identity",
		description: "SOUL.md",
		detail:
			"Defines the agent's personality, values, communication style, and behavioral boundaries. This is the core of who the agent is.",
	},
	{
		id: "identity",
		label: "Identity",
		group: "identity",
		description: "IDENTITY.md",
		detail:
			"The agent's name, nature, and purpose. How it introduces itself and what it understands its role to be.",
	},
	{
		id: "role",
		label: "Role",
		group: "identity",
		description: "ROLE.md",
		detail:
			"The agent's responsibilities, scope, expected outcomes, and escalation rules. Defines what the agent does and doesn't do.",
	},
	{
		id: "routing",
		label: "Model Routing",
		group: "config",
		description: "Which models each process uses",
		detail:
			"Controls which LLM model is used for each process type. Channels handle user-facing conversation, branches do thinking, workers execute tasks, the compactor summarizes context, cortex observes system state, and voice transcribes audio attachments before the channel turn.",
	},
	{
		id: "tuning",
		label: "Tuning",
		group: "config",
		description: "Turn limits, context window, branches",
		detail:
			"Core limits that control how much work the agent does per message. Max turns caps LLM iterations per channel message. Context window sets the token budget. Branch limits control parallel thinking.",
	},
	{
		id: "compaction",
		label: "Compaction",
		group: "config",
		description: "Context compaction thresholds",
		detail:
			"Thresholds that trigger context summarization as the conversation grows. Background kicks in early, aggressive compresses harder, and emergency truncates without LLM involvement. All values are fractions of the context window.",
	},
	{
		id: "cortex",
		label: "Cortex",
		group: "config",
		description: "System observer settings",
		detail:
			"The cortex monitors active processes and generates memory bulletins. Tick interval controls observation frequency. Timeouts determine when stuck workers or branches get cancelled. The circuit breaker auto-disables after consecutive failures.",
	},
	{
		id: "coalesce",
		label: "Coalesce",
		group: "config",
		description: "Message batching",
		detail:
			"When multiple messages arrive in quick succession, coalescing batches them into a single LLM turn. This prevents the agent from responding to each message individually in fast-moving conversations.",
	},
	{
		id: "memory",
		label: "Memory Persistence",
		group: "config",
		description: "Auto-save interval",
		detail:
			"Spawns a silent background branch at regular intervals to recall existing memories and save new ones from the recent conversation. Runs without blocking the channel.",
	},
	{
		id: "browser",
		label: "Browser",
		group: "config",
		description: "Chrome automation",
		detail:
			"Controls browser automation tools available to workers. When enabled, workers can navigate web pages, take screenshots, and interact with sites. JavaScript evaluation is a separate permission.",
	},
	{
		id: "sandbox",
		label: "Sandbox",
		group: "config",
		description: "Process containment",
		detail:
			"OS-level filesystem containment for shell tool subprocesses. When enabled, worker processes run inside a kernel-enforced sandbox (bubblewrap on Linux, sandbox-exec on macOS) with an allowlist-only filesystem — only system paths, the workspace, and explicitly configured extra paths are accessible.",
	},
	{
		id: "projects",
		label: "Projects",
		group: "config",
		description: "Workspace management",
		detail:
			"Controls how the agent manages project workspaces, git repos, and worktrees. Use worktrees for parallel feature branches, auto-discover to scan for repos on project creation, and set a disk usage warning threshold.",
	},
	{
		id: "ingest",
		label: "Ingest",
		group: "data",
		description: "File upload & processing",
		detail:
			"Upload documents to be chunked and processed into structured memories. Supported formats include PDF, text, markdown, JSON, CSV, YAML, TOML, HTML, and more.",
	},
];

export const GRADIENT_PRESETS: {label: string; start: string; end: string}[] = [
	{label: "Purple", start: "hsl(270, 70%, 55%)", end: "hsl(310, 60%, 45%)"},
	{label: "Blue", start: "hsl(220, 70%, 55%)", end: "hsl(260, 60%, 45%)"},
	{label: "Teal", start: "hsl(170, 70%, 45%)", end: "hsl(200, 60%, 40%)"},
	{label: "Green", start: "hsl(140, 60%, 45%)", end: "hsl(170, 50%, 40%)"},
	{label: "Orange", start: "hsl(25, 80%, 55%)", end: "hsl(45, 70%, 45%)"},
	{label: "Red", start: "hsl(0, 70%, 55%)", end: "hsl(20, 60%, 45%)"},
	{label: "Pink", start: "hsl(330, 70%, 55%)", end: "hsl(350, 60%, 45%)"},
	{label: "Gold", start: "hsl(45, 80%, 55%)", end: "hsl(30, 70%, 40%)"},
	{label: "Indigo", start: "hsl(240, 60%, 55%)", end: "hsl(280, 50%, 45%)"},
	{label: "Slate", start: "hsl(220, 15%, 50%)", end: "hsl(220, 10%, 35%)"},
];

export const SANDBOX_DEFAULTS = {
	mode: "enabled" as const,
	writable_paths: [] as string[],
};
