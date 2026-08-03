import type {SectionId} from "./types";

export const SECTIONS = [
	{
		id: "instance" as const,
		label: "Instance",
		group: "general" as const,
		description: "Instance name and identity",
	},
	{
		id: "providers" as const,
		label: "Providers",
		group: "general" as const,
		description: "LLM provider credentials",
	},
	{
		id: "channels" as const,
		label: "Channels",
		group: "messaging" as const,
		description: "Messaging platforms and bindings",
	},
	{
		id: "api-keys" as const,
		label: "API Keys",
		group: "general" as const,
		description: "Third-party service keys",
	},
	{
		id: "secrets" as const,
		label: "Secrets",
		group: "general" as const,
		description: "Encrypted secret storage",
	},
	{
		id: "server" as const,
		label: "Server",
		group: "system" as const,
		description: "API server configuration",
	},
	{
		id: "opencode" as const,
		label: "OpenCode",
		group: "system" as const,
		description: "OpenCode worker integration",
	},
	{
		id: "worker-logs" as const,
		label: "Worker Logs",
		group: "system" as const,
		description: "Worker execution logging",
	},
	{
		id: "updates" as const,
		label: "Updates",
		group: "system" as const,
		description: "Release checks and update controls",
	},
	{
		id: "appearance" as const,
		label: "Appearance",
		group: "general" as const,
		description: "Theme and display settings",
	},
	{
		id: "config-file" as const,
		label: "Config File",
		group: "system" as const,
		description: "Raw config.toml editor",
	},
	{
		id: "changelog" as const,
		label: "Changelog",
		group: "system" as const,
		description: "Release history",
	},
] satisfies {
	id: SectionId;
	label: string;
	group: string;
	description: string;
}[];

/// The two API dialects a provider can speak.
///
/// There is no per-vendor list any more. Adding OpenRouter, Groq, or a
/// self-hosted vLLM is the same form with a different base URL, so hardcoding
/// twenty vendors bought nothing but twenty things to keep current.
export const API_TYPES = [
	{
		id: "openai_compatible" as const,
		label: "OpenAI-compatible",
		description:
			"Any endpoint that speaks /chat/completions — LiteLLM, vLLM, Ollama, OpenRouter, OpenAI, TGI.",
		baseUrlPlaceholder: "http://localhost:4000/v1",
		baseUrlHint:
			"Full path prefix. Nothing is appended but the endpoint, so include /v1 if your server expects it.",
		keyPlaceholder: "sk-...",
	},
	{
		id: "anthropic" as const,
		label: "Anthropic (native)",
		description:
			"Anthropic's Messages API. Required for prompt caching, extended thinking, and Claude Pro/Max OAuth.",
		baseUrlPlaceholder: "https://api.anthropic.com",
		baseUrlHint: "Defaults to https://api.anthropic.com.",
		keyPlaceholder: "sk-ant-...",
	},
] satisfies {
	id: string;
	label: string;
	description: string;
	baseUrlPlaceholder: string;
	baseUrlHint: string;
	keyPlaceholder: string;
}[];

export type ApiTypeId = (typeof API_TYPES)[number]["id"];

/// Starting point for a new provider. LiteLLM is the recommended way to reach
/// anything that is not Anthropic, but it is a suggestion, not a dependency.
export const DEFAULT_NEW_PROVIDER = {
	id: "litellm",
	apiType: "openai_compatible" as ApiTypeId,
	baseUrl: "http://localhost:4000/v1",
};

export const PERMISSION_OPTIONS = [
	{
		value: "allow",
		label: "Allow",
		description: "Tool can run without restriction",
	},
	{value: "deny", label: "Deny", description: "Tool is completely disabled"},
];
