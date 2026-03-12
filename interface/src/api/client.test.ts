import {afterEach, beforeEach, describe, expect, mock, test} from "bun:test";

if (!("window" in globalThis)) {
	(globalThis as typeof globalThis & {window: Record<string, unknown>}).window = {};
}

const {api} = await import("./client");

const originalFetch = globalThis.fetch;

describe("api client runtime health actions", () => {
	beforeEach(() => {
		globalThis.fetch = mock(async (_input: RequestInfo | URL, _init?: RequestInit) => {
			return new Response(JSON.stringify({ success: true, status: "accepted", forced: true, accepted_agents: ["main"], message: "ok" }), {
				status: 200,
				headers: { "Content-Type": "application/json" },
			});
		}) as typeof fetch;
	});

	afterEach(() => {
		globalThis.fetch = originalFetch;
	});

	test("triggerWarmup posts agent and force payload", async () => {
		await api.triggerWarmup({ agentId: "main", force: true });

		expect(globalThis.fetch).toHaveBeenCalledTimes(1);
		expect(globalThis.fetch).toHaveBeenCalledWith(
			"/api/agents/warmup",
			expect.objectContaining({
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ agent_id: "main", force: true }),
			}),
		);
	});

	test("triggerWarmup defaults to all agents without force", async () => {
		await api.triggerWarmup();

		expect(globalThis.fetch).toHaveBeenCalledTimes(1);
		expect(globalThis.fetch).toHaveBeenCalledWith(
			"/api/agents/warmup",
			expect.objectContaining({
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ agent_id: null, force: false }),
			}),
		);
	});

	test("reconnectMcpServer posts to the reconnect endpoint", async () => {
		await api.reconnectMcpServer("filesystem");

		expect(globalThis.fetch).toHaveBeenCalledTimes(1);
		expect(globalThis.fetch).toHaveBeenCalledWith(
			"/api/mcp/servers/filesystem/reconnect",
			expect.objectContaining({
				method: "POST",
			}),
		);
	});
});
