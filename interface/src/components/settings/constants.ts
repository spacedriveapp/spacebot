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
		id: "prompt-debug" as const,
		label: "Prompt Capture",
		group: "system" as const,
		description: "Record LLM requests for the prompt inspector",
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
		id: "composition" as const,
		label: "Composition",
		group: "general" as const,
		description: "Chat composer and message input behavior",
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

export const PROVIDERS = [
	{
		id: "openrouter",
		name: "OpenRouter",
		description: "Multi-provider gateway with unified API",
		placeholder: "sk-or-...",
		envVar: "OPENROUTER_API_KEY",
	},
	{
		id: "kilo",
		name: "Kilo Gateway",
		description: "OpenAI-compatible multi-provider gateway",
		placeholder: "sk-...",
		envVar: "KILO_API_KEY",
	},
	{
		id: "opencode-zen",
		name: "OpenCode Zen",
		description: "Multi-format gateway (Kimi, GLM, MiniMax, Qwen)",
		placeholder: "...",
		envVar: "OPENCODE_ZEN_API_KEY",
	},
	{
		id: "opencode-go",
		name: "OpenCode Go",
		description: "Lite OpenCode model catalog and limits",
		placeholder: "...",
		envVar: "OPENCODE_GO_API_KEY",
	},
	{
		id: "anthropic",
		name: "Anthropic",
		description: "Claude models (Sonnet, Opus, Haiku)",
		placeholder: "sk-ant-...",
		envVar: "ANTHROPIC_API_KEY",
	},
	{
		id: "openai",
		name: "OpenAI",
		description: "GPT models",
		placeholder: "sk-...",
		envVar: "OPENAI_API_KEY",
	},
	{
		id: "zai-coding-plan",
		name: "Z.AI Coding Plan",
		description: "GLM coding models (glm-4.7, glm-5, glm-4.5-air)",
		placeholder: "...",
		envVar: "ZAI_CODING_PLAN_API_KEY",
	},
	{
		id: "zhipu",
		name: "Z.ai (GLM)",
		description: "GLM models (GLM-4, GLM-4-Flash)",
		placeholder: "...",
		envVar: "ZHIPU_API_KEY",
	},
	{
		id: "groq",
		name: "Groq",
		description: "Fast inference for Llama, Mixtral models",
		placeholder: "gsk_...",
		envVar: "GROQ_API_KEY",
	},
	{
		id: "together",
		name: "Together AI",
		description: "Wide model selection with competitive pricing",
		placeholder: "...",
		envVar: "TOGETHER_API_KEY",
	},
	{
		id: "fireworks",
		name: "Fireworks AI",
		description: "Fast inference for popular OSS models",
		placeholder: "...",
		envVar: "FIREWORKS_API_KEY",
	},
	{
		id: "deepseek",
		name: "DeepSeek",
		description: "DeepSeek Chat and Reasoner models",
		placeholder: "sk-...",
		envVar: "DEEPSEEK_API_KEY",
	},
	{
		id: "xai",
		name: "xAI",
		description: "Grok models",
		placeholder: "xai-...",
		envVar: "XAI_API_KEY",
	},
	{
		id: "mistral",
		name: "Mistral AI",
		description: "Mistral Large, Small, Codestral models",
		placeholder: "...",
		envVar: "MISTRAL_API_KEY",
	},
	{
		id: "gemini",
		name: "Google Gemini",
		description: "Google Gemini experimental and production models",
		placeholder: "AIza...",
		envVar: "GEMINI_API_KEY",
	},
	{
		id: "nvidia",
		name: "NVIDIA NIM",
		description: "NVIDIA-hosted models via NIM API",
		placeholder: "nvapi-...",
		envVar: "NVIDIA_API_KEY",
	},
	{
		id: "minimax",
		name: "MiniMax",
		description: "MiniMax (Anthropic message format)",
		placeholder: "sk-...",
		envVar: "MINIMAX_API_KEY",
	},
	{
		id: "minimax-cn",
		name: "MiniMax CN",
		description: "MiniMax China (Anthropic message format)",
		placeholder: "sk-...",
		envVar: "MINIMAX_CN_API_KEY",
	},
	{
		id: "moonshot",
		name: "Moonshot AI",
		description: "Kimi models (Kimi K2, Kimi K2.5)",
		placeholder: "sk-...",
		envVar: "MOONSHOT_API_KEY",
	},
	{
		id: "github-copilot",
		name: "GitHub Copilot",
		description: "GitHub Copilot API (uses GitHub PAT for token exchange)",
		placeholder: "ghp_... or gh auth token",
		envVar: "GITHUB_COPILOT_API_KEY",
	},
	{
		id: "azure",
		name: "Azure OpenAI",
		description: "Azure OpenAI Service with custom deployments",
		placeholder: "Azure API key (alphanumeric string)",
		envVar: "AZURE_API_KEY",
	},
	{
		id: "ollama",
		name: "Ollama",
		description: "Local or remote Ollama API endpoint",
		placeholder: "http://localhost:11434",
		envVar: "OLLAMA_BASE_URL",
	},
] as const;

export const PERMISSION_OPTIONS = [
	{
		value: "allow",
		label: "Allow",
		description: "Tool can run without restriction",
	},
	{value: "deny", label: "Deny", description: "Tool is completely disabled"},
];
