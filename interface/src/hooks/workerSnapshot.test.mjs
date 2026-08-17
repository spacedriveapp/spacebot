import {describe, expect, test} from "bun:test";
import {reconcileWorkerSnapshot, workerLifecycleKey} from "./workerSnapshot.ts";

function reconcile(current, snapshot, requestGeneration, lifecycleGenerations) {
	return reconcileWorkerSnapshot(
		current,
		snapshot,
		requestGeneration,
		lifecycleGenerations,
		(worker) => worker.agentId === "agent-a",
		(worker) => workerLifecycleKey(worker.agentId, worker.id),
		(worker) => worker.id,
		(worker) => workerLifecycleKey(worker.agentId, worker.id),
		(_current, worker) => worker,
	);
}

describe("worker snapshot reconciliation", () => {
	test("does not resurrect a worker completed during the request", () => {
		const staleWorker = {id: "worker-a", registrationId: "1", agentId: "agent-a"};
		const generations = new Map([[workerLifecycleKey("agent-a", "worker-a"), 2]]);

		expect(reconcile({}, [staleWorker], 1, generations)).toEqual({});
	});

	test("preserves a replacement registration created during the request", () => {
		const replacement = {id: "worker-a", registrationId: "2", agentId: "agent-a"};
		const staleWorker = {id: "worker-a", registrationId: "1", agentId: "agent-a"};
		const generations = new Map([[workerLifecycleKey("agent-a", "worker-a"), 2]]);

		expect(reconcile({"worker-a": replacement}, [staleWorker], 1, generations)).toEqual({
			"worker-a": replacement,
		});
	});
});
