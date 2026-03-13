import {describe, expect, test} from "bun:test";
import type {
	McpAgentStatus,
	MessagingStatusResponse,
	ProvidersResponse,
	SecretStoreStatus,
	WarmupStatusResponse,
} from "@/api/client";

if (!("window" in globalThis)) {
	(globalThis as typeof globalThis & {window: Window & typeof globalThis}).window =
		globalThis as Window & typeof globalThis;
}

const {classifySetupReadiness} = await import("./SetupReadiness");

function configuredProviders(hasAny = true): ProvidersResponse {
	return {
		has_any: hasAny,
		providers: {
			anthropic: hasAny,
			openai: false,
			openai_chatgpt: false,
			openrouter: false,
			kilo: false,
			zhipu: false,
			groq: false,
			together: false,
			fireworks: false,
			deepseek: false,
			xai: false,
			mistral: false,
			gemini: false,
			ollama: false,
			opencode_zen: false,
			opencode_go: false,
			nvidia: false,
			minimax: false,
			minimax_cn: false,
			moonshot: false,
			zai_coding_plan: false,
			github_copilot: false,
		},
	};
}

function secretsStatus(overrides: Partial<SecretStoreStatus> = {}): SecretStoreStatus {
	return {
		state: "unlocked",
		encrypted: true,
		secret_count: 0,
		system_count: 0,
		tool_count: 0,
		platform_managed: false,
		...overrides,
	};
}

function messagingStatus(
	instances: MessagingStatusResponse["instances"] = [],
): MessagingStatusResponse {
	return {
		discord: {configured: false, enabled: false},
		slack: {configured: false, enabled: false},
		telegram: {configured: false, enabled: false},
		webhook: {configured: false, enabled: false},
		twitch: {configured: false, enabled: false},
		email: {configured: false, enabled: false},
		instances,
	};
}

function warmupStatus(
	statuses: WarmupStatusResponse["statuses"],
): WarmupStatusResponse {
	return {statuses};
}

function mcpStatus(servers: McpAgentStatus["servers"]): McpAgentStatus[] {
	return [{agent_id: "main", servers}];
}

describe("classifySetupReadiness", () => {
	test("flags missing provider as a blocker", () => {
		const items = classifySetupReadiness({
			providers: configuredProviders(false),
		});

		expect(items).toEqual([
			expect.objectContaining({
				id: "provider",
				severity: "blocker",
				tab: "providers",
			}),
		]);
	});

	test("flags locked and unencrypted secrets separately", () => {
		const lockedItems = classifySetupReadiness({
			providers: configuredProviders(),
			secrets: secretsStatus({state: "locked", encrypted: true, secret_count: 2}),
		});
		expect(lockedItems).toContainEqual(
			expect.objectContaining({
				id: "secrets_locked",
				severity: "warning",
				tab: "secrets",
			}),
		);

		const unencryptedItems = classifySetupReadiness({
			providers: configuredProviders(),
			secrets: secretsStatus({encrypted: false, secret_count: 2}),
		});
		expect(unencryptedItems).toContainEqual(
			expect.objectContaining({
				id: "secrets_unencrypted",
				severity: "warning",
				tab: "secrets",
			}),
		);
	});

	test("reports degraded warmup whenever warmup data is present", () => {
		const items = classifySetupReadiness({
			providers: configuredProviders(),
			warmup: warmupStatus([
				{
					agent_id: "alpha",
					status: {
						state: "degraded",
						embedding_ready: false,
						last_refresh_unix_ms: null,
						last_error: "boom",
						bulletin_age_secs: null,
					},
				},
			]),
		});

		expect(items).toContainEqual(
			expect.objectContaining({
				id: "warmup_degraded",
				severity: "warning",
				tab: "system-health",
			}),
		);

		const noProviderItems = classifySetupReadiness({
			warmup: warmupStatus([
				{
					agent_id: "alpha",
					status: {
						state: "degraded",
						embedding_ready: false,
						last_refresh_unix_ms: null,
						last_error: "boom",
						bulletin_age_secs: null,
					},
				},
			]),
		});

		expect(noProviderItems).toContainEqual(
			expect.objectContaining({
				id: "warmup_degraded",
				severity: "warning",
				tab: "system-health",
			}),
		);
	});

	test("reports disconnected MCP servers and ignores disabled ones", () => {
		const items = classifySetupReadiness({
			providers: configuredProviders(),
			mcp: mcpStatus([
				{name: "filesystem", transport: "stdio", enabled: true, state: "failed: timeout"},
				{name: "disabled", transport: "stdio", enabled: false, state: "failed: ignored"},
			]),
		});

		expect(items).toContainEqual(
			expect.objectContaining({
				id: "mcp_failed",
				severity: "warning",
				tab: "system-health",
			}),
		);
		expect(items.some((item) => item.description.includes("2 enabled MCP servers"))).toBe(false);
	});

	test("routes settling runtime signals to system health", () => {
		const warmupItems = classifySetupReadiness({
			providers: configuredProviders(),
			warmup: warmupStatus([
				{
					agent_id: "alpha",
					status: {
						state: "cold",
						embedding_ready: false,
						last_refresh_unix_ms: null,
						last_error: null,
						bulletin_age_secs: null,
					},
				},
			]),
		});

		expect(warmupItems).toContainEqual(
			expect.objectContaining({
				id: "warmup_pending",
				severity: "info",
				tab: "system-health",
			}),
		);

		const mcpItems = classifySetupReadiness({
			providers: configuredProviders(),
			mcp: mcpStatus([
				{name: "filesystem", transport: "stdio", enabled: true, state: "connecting"},
			]),
		});

		expect(mcpItems).toContainEqual(
			expect.objectContaining({
				id: "mcp_connecting",
				severity: "info",
				tab: "system-health",
			}),
		);
	});

	test("reports messaging setup and binding gaps as informational", () => {
		const missingItems = classifySetupReadiness({
			providers: configuredProviders(),
			messaging: messagingStatus(),
		});

		expect(missingItems).toContainEqual(
			expect.objectContaining({
				id: "messaging_missing",
				severity: "info",
				tab: "channels",
			}),
		);

		const unboundItems = classifySetupReadiness({
			providers: configuredProviders(),
			messaging: messagingStatus([
				{
					platform: "discord",
					name: null,
					runtime_key: "discord",
					configured: true,
					enabled: true,
					binding_count: 0,
				},
			]),
		});

		expect(unboundItems).toContainEqual(
			expect.objectContaining({
				id: "messaging_unbound",
				severity: "info",
				tab: "channels",
			}),
		);

		const partiallyBoundItems = classifySetupReadiness({
			providers: configuredProviders(),
			messaging: messagingStatus([
				{
					platform: "discord",
					name: "ops",
					runtime_key: "discord:ops",
					configured: true,
					enabled: true,
					binding_count: 2,
				},
				{
					platform: "telegram",
					name: "alerts",
					runtime_key: "telegram:alerts",
					configured: true,
					enabled: true,
					binding_count: 0,
				},
			]),
		});

		expect(partiallyBoundItems).toContainEqual(
			expect.objectContaining({
				id: "messaging_unbound",
				severity: "info",
				tab: "channels",
				description: "Some configured platforms still need bindings before they deliver conversations to an agent.",
			}),
		);
	});

	test("surfaces probe errors without suppressing other readiness items", () => {
		const items = classifySetupReadiness({
			providers: configuredProviders(false),
			probeErrors: ["warmup", "mcp"],
		});

		expect(items).toContainEqual(
			expect.objectContaining({
				id: "probe_error",
				severity: "warning",
			}),
		);
		expect(items).toContainEqual(
			expect.objectContaining({
				id: "provider",
				severity: "blocker",
				tab: "providers",
			}),
		);
	});
});
