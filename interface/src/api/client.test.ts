import {afterEach, beforeEach, describe, expect, mock, test} from "bun:test";

if (!("window" in globalThis)) {
	(globalThis as typeof globalThis & {window: Window & typeof globalThis}).window =
		globalThis as Window & typeof globalThis;
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
			}) as unknown as typeof fetch;
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

	test("reconnectMcpServer posts the agent-scoped reconnect payload", async () => {
		await api.reconnectMcpServer({ agentId: "main", serverName: "filesystem" });

		expect(globalThis.fetch).toHaveBeenCalledTimes(1);
		expect(globalThis.fetch).toHaveBeenCalledWith(
			"/api/agents/mcp/reconnect",
			expect.objectContaining({
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ agent_id: "main", server_name: "filesystem" }),
				}),
			);
		});

		test("triggerWarmup includes response text in thrown API errors", async () => {
			globalThis.fetch = mock(async (_input: RequestInfo | URL, _init?: RequestInit) => {
				return new Response("warmup unavailable", { status: 503 });
			}) as unknown as typeof fetch;

			await expect(api.triggerWarmup({ agentId: "main" })).rejects.toThrow(
				"API error: 503: warmup unavailable",
			);
		});

		test("reconnectMcpServer includes response text in thrown API errors", async () => {
			globalThis.fetch = mock(async (_input: RequestInfo | URL, _init?: RequestInit) => {
				return new Response("reconnect failed", { status: 500 });
			}) as unknown as typeof fetch;

			await expect(
				api.reconnectMcpServer({ agentId: "main", serverName: "filesystem" }),
			).rejects.toThrow("API error: 500: reconnect failed");
		});
	});
