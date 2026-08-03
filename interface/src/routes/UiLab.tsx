/**
 * Development-only visual harness.
 *
 * Renders task components against fixtures so they can be inspected without a
 * running backend. Not linked from navigation; reachable at /__uilab.
 */
import { useState } from "react";
import type {
	TaskContractResponse,
	TaskProvenanceResponse,
	TaskDependenciesResponse,
	TaskEdgeSummary,
	TaskItem,
	TaskRun,
} from "@/api/client";
import { BlockedTasksSection } from "@/components/tasks/BlockedTasksSection";
import { TaskRunHistoryView } from "@/components/tasks/TaskRunHistory";
import { RepoChip, type BindingNames } from "@/components/tasks/RepoChip";
import { ALL_REPOS, RepoFilter } from "@/components/tasks/RepoFilter";
import { DependencySectionView } from "@/components/tasks/DependencySection";
import { ContractSectionView } from "@/components/tasks/ContractSection";
import { ProvenanceSectionView } from "@/components/tasks/ProvenanceSection";
import { FailureBudgetSectionView } from "@/components/tasks/FailureBudgetSection";
import { indexEdges } from "@/components/tasks/DependencyBadges";

const BINDING_NAMES: BindingNames = {
	projects: new Map([["proj-platform", "platform"]]),
	repos: new Map([
		["repo-api", "api-gateway"],
		["repo-web", "web"],
		["repo-auth", "auth-service"],
	]),
	worktrees: new Map([["wt-feature", "feat/contract-v2"]]),
};

const AGENTS: Record<string, string> = {
	"agent-platform": "Platform Agent",
	"agent-web": "Web Agent",
};

// Counted rather than random. `crypto.randomUUID` is undefined outside a
// secure context, so calling it throws as soon as the app is reached over a
// LAN or tailnet address instead of localhost — `lib/id.ts::generateId` exists
// for exactly that reason and is the right choice for real client-side ids.
// Fixtures want stable ids across reloads more than they want unique ones,
// so a counter beats both.
let nextFixtureId = 0;

function fixtureTask(overrides: Partial<TaskItem>): TaskItem {
	nextFixtureId += 1;
	return {
		id: `fixture-${nextFixtureId}`,
		task_number: 1,
		title: "Untitled",
		status: "blocked",
		priority: "medium",
		owner_agent_id: "agent-platform",
		assigned_agent_id: "agent-platform",
		subtasks: [],
		metadata: {},
		created_by: "cortex",
		created_at: new Date().toISOString(),
		updated_at: new Date().toISOString(),
		consecutive_failures: 0,
		block_recurrences: 0,
		fan_out_placeholder: false,
		loop_terminal: false,
		...overrides,
	};
}

const BLOCKED: TaskItem[] = [
	fixtureTask({
		task_number: 142,
		title: "Regenerate API clients after contract change",
		assigned_agent_id: "agent-web",
		consecutive_failures: 2,
		max_retries: 2,
		project_id: "proj-platform",
		repo_id: "repo-web",
		block_kind: "transient",
		block_reason:
			"worker exceeded 1800s wall-clock timeout after 10 segments. Last tool call: shell(`bun run codegen`) — no output for 22m.",
	}),
	fixtureTask({
		task_number: 138,
		title: "Rotate the staging database credentials",
		consecutive_failures: 2,
		project_id: "proj-platform",
		repo_id: "repo-auth",
		block_kind: "capability",
		block_reason: "no secret named STAGING_DB_URL is available to this agent",
	}),
	fixtureTask({
		task_number: 96,
		title: "Backfill wiki pages for the ingestion subsystem",
		priority: "low",
		consecutive_failures: 0,
		block_kind: "needs_input",
		block_reason:
			"two candidate page hierarchies — flat per-module, or nested by subsystem. Which?",
	}),
];

const EDGES: TaskEdgeSummary[] = [
	{task_number: 142, parents: 3, children: 2, blocked_by: 1},
	{task_number: 138, parents: 0, children: 4, blocked_by: 0},
];

const DEPENDENCIES: TaskDependenciesResponse = {
	parents: [128, 131, 133],
	children: [151, 152],
	blocked_by: [133],
};

const RUNS: TaskRun[] = [
	{
		id: "r1",
		task_number: 142,
		attempt: 1,
		worker_id: "9f3a2b1c-4d5e-6f70-8901-234567890abc",
		outcome: "failed",
		error: "connection refused talking to the codegen sidecar on 127.0.0.1:4010",
		started_at: "2026-08-02T09:14:02Z",
		ended_at: "2026-08-02T09:16:41Z",
	},
	{
		id: "r2",
		task_number: 142,
		attempt: 2,
		worker_id: "1a2b3c4d-5e6f-7081-9234-567890abcdef",
		outcome: "rate_limited",
		error: "429 Too Many Requests — provider quota exhausted, retrying without spending budget",
		started_at: "2026-08-02T09:20:00Z",
		ended_at: "2026-08-02T09:20:12Z",
	},
	{
		id: "r3",
		task_number: 142,
		attempt: 3,
		worker_id: "abcdef01-2345-6789-abcd-ef0123456789",
		outcome: "timeout",
		error: "worker exceeded 1800s wall-clock timeout after 10 segments",
		started_at: "2026-08-02T09:25:00Z",
		ended_at: "2026-08-02T09:55:00Z",
	},
	{
		id: "r3b",
		task_number: 142,
		attempt: 4,
		worker_id: "deadbeef-0000-1111-2222-333344445555",
		outcome: "abandoned",
		error:
			"worker deadbeef-0000-1111-2222-333344445555 is gone without reporting an outcome — the process most likely died or the agent restarted mid-run",
		started_at: "2026-08-02T09:58:00Z",
		ended_at: "2026-08-02T10:01:00Z",
	},
	{
		id: "r4",
		task_number: 142,
		attempt: 5,
		worker_id: "55555555-6666-7777-8888-999999999999",
		outcome: "completed",
		summary:
			"Regenerated 4 client packages, ran the contract test suite (18 passed), and opened PR #331 against web.",
		started_at: "2026-08-02T10:02:00Z",
		ended_at: "2026-08-02T10:09:30Z",
	},
	{
		id: "r5",
		task_number: 142,
		attempt: 6,
		worker_id: "77777777-8888-9999-aaaa-bbbbbbbbbbbb",
		started_at: "2026-08-02T10:30:00Z",
	},
];

/** A healthy contract: every input resolved, output produced. */
const CONTRACT_OK: TaskContractResponse = {
	input_schema: {
		type: "object",
		required: ["tag", "environment"],
		properties: {tag: {type: "string"}, environment: {type: "string"}},
	},
	output_schema: {
		type: "object",
		required: ["deployment_url"],
		properties: {deployment_url: {type: "string"}},
	},
	inputs: {tag: "v1.4.2", environment: "staging"},
	resolved_inputs: {tag: "v1.4.2", environment: "staging"},
	outputs: {deployment_url: "https://staging.platform.internal"},
	bindings: [
		{
			child_task_number: 151,
			input_key: "tag",
			source_task_number: 142,
			source_pointer: "/image/tag",
			literal_value: null,
		},
		{
			child_task_number: 151,
			input_key: "environment",
			source_task_number: null,
			source_pointer: null,
			literal_value: "staging",
		},
	],
	problems: [],
};

/** The case that matters: the graph cannot supply what the task was promised. */
const CONTRACT_BROKEN: TaskContractResponse = {
	input_schema: CONTRACT_OK.input_schema,
	output_schema: CONTRACT_OK.output_schema,
	inputs: null,
	resolved_inputs: {environment: "production"},
	outputs: null,
	bindings: [
		{
			child_task_number: 152,
			input_key: "tag",
			source_task_number: 142,
			source_pointer: "/image/tag",
			literal_value: null,
		},
		{
			child_task_number: 152,
			input_key: "environment",
			source_task_number: null,
			source_pointer: null,
			literal_value: "production",
		},
	],
	problems: [
		{
			kind: "pointer_missed",
			input_key: "tag",
			source_task_number: 142,
			pointer: "/image/tag",
		},
	],
};

const PROVENANCE: TaskProvenanceResponse = {
	filed_by_task_number: 128,
	remaining_fan_out: 7,
	filed: [
		fixtureTask({
			task_number: 151,
			title: "Regenerate the web client",
			status: "ready",
		}),
		fixtureTask({
			task_number: 152,
			title: "Regenerate the mobile client",
			status: "backlog",
			block_kind: "dependency",
		}),
		fixtureTask({
			task_number: 153,
			title: "Update the integration tests",
			status: "done",
		}),
	],
};

export function UiLab() {
	const [collapsed, setCollapsed] = useState(false);
	const [retrying, setRetrying] = useState<number | null>(null);
	const [repo, setRepo] = useState<string>(ALL_REPOS);

	return (
		<div className="min-h-screen bg-app p-8">
			<h1 className="mb-1 font-plex text-lg font-semibold text-ink">UI Lab</h1>
			<p className="mb-6 text-xs text-ink-faint">
				Development harness — task components rendered against fixtures.
			</p>

			<section className="mb-10">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					BlockedTasksSection
				</h2>
				<div className="overflow-hidden rounded-md border border-app-line bg-app-box/20">
					<BlockedTasksSection
						tasks={BLOCKED}
						collapsed={collapsed}
						onToggle={() => setCollapsed((v) => !v)}
						onRetry={(task) => {
							setRetrying(task.task_number);
							setTimeout(() => setRetrying(null), 1500);
						}}
						retryingTaskNumber={retrying}
						resolveAgentName={(id) => AGENTS[id] ?? id}
						bindingNames={BINDING_NAMES}
						edges={indexEdges(EDGES)}
						onUnblock={() => {}}
					/>
				</div>
			</section>

			<section className="mb-10">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					RepoChip / RepoFilter
				</h2>
				<div className="flex flex-wrap items-center gap-3 rounded-md border border-app-line bg-app-box/20 p-3">
					<RepoFilter
						names={BINDING_NAMES}
						value={repo}
						onChange={setRepo}
						presentRepoIds={new Set(["repo-api", "repo-web", "repo-auth"])}
					/>
					<span className="text-[11px] text-ink-faint">selected: {repo}</span>
					<div className="flex items-center gap-2">
						<RepoChip
							task={{project_id: "proj-platform", repo_id: "repo-api", worktree_id: null}}
							names={BINDING_NAMES}
						/>
						<RepoChip
							task={{
								project_id: "proj-platform",
								repo_id: "repo-api",
								worktree_id: "wt-feature",
							}}
							names={BINDING_NAMES}
						/>
						<RepoChip
							task={{project_id: "proj-platform", repo_id: null, worktree_id: null}}
							names={BINDING_NAMES}
						/>
						<span className="text-[11px] text-ink-faint">
							(unbound renders nothing →)
						</span>
						<RepoChip
							task={{project_id: null, repo_id: null, worktree_id: null}}
							names={BINDING_NAMES}
						/>
					</div>
				</div>
			</section>

			<section className="mb-10 max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					DependencySection
				</h2>
				<div className="rounded-md border border-app-line bg-app-box/20">
					<DependencySectionView
						data={DEPENDENCIES}
						onSelectTask={() => {}}
					/>
				</div>
			</section>

			<section className="mb-10 max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					ContractSection — satisfied
				</h2>
				<div className="rounded-md border border-app-line bg-app-box/20">
					<ContractSectionView data={CONTRACT_OK} onSelectTask={() => {}} />
				</div>
			</section>

			<section className="mb-10 max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					ContractSection — unresolved
				</h2>
				<div className="rounded-md border border-app-line bg-app-box/20">
					<ContractSectionView data={CONTRACT_BROKEN} onSelectTask={() => {}} />
				</div>
			</section>

			<section className="mb-10 max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					ProvenanceSection
				</h2>
				<div className="rounded-md border border-app-line bg-app-box/20">
					<ProvenanceSectionView data={PROVENANCE} onSelectTask={() => {}} />
				</div>
			</section>

			<section className="mb-10 max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					FailureBudgetSection — default, overridden, spent
				</h2>
				<div className="space-y-2">
					{[
						{budget: null, failures: 0, parked: false},
						{budget: 5, failures: 2, parked: false},
						{budget: 1, failures: 1, parked: true},
						{budget: 24, failures: 3, parked: false},
					].map((fixture) => (
						<div
							key={`${fixture.budget}-${fixture.failures}`}
							className="rounded-md border border-app-line bg-app-box/20"
						>
							<FailureBudgetSectionView
								budget={fixture.budget}
								failures={fixture.failures}
								parked={fixture.parked}
								onChange={() => {}}
							/>
						</div>
					))}
				</div>
			</section>

			<section className="max-w-2xl">
				<h2 className="mb-2 font-mono text-xs font-semibold tracking-wide text-ink-dull">
					TaskRunHistory
				</h2>
				<div className="rounded-md border border-app-line bg-app-box/20 p-3">
					<TaskRunHistoryView runs={RUNS} />
				</div>
			</section>
		</div>
	);
}
